# RESEARCH — Near-instant TTS and Bidirectional Voice Control for crabcc (May 2026)

> **Audience:** crabcc maintainers (Rust workspace + Flutter mobile + cloud relay).
> **Issues:** #239 (Piper-over-QUIC TTS path), #240 (mobile voice client), #242 (full-duplex voice control).
> **Tone:** opinionated. The point of this document is to make decisions, not survey the field.

---

## TL;DR

1. **Piper over QUIC is fine for v0, but it's a dead end.** Piper's VITS-based architecture is non-streaming at the model level — you wait for the entire mel-spectrogram before the vocoder can start. That puts a structural floor on TTFA (~250–400 ms on a Pi 4, ~120–180 ms on M-series silicon) that no transport optimization will fix. **For v1 ship Kyutai TTS 1.6B or Pocket TTS** (delayed-streams modeling, sub-220 ms end-to-end) on the OSS lane and **Cartesia Sonic-3** (~90 ms TTFA) on the cloud lane.
2. **For input, Voxtral Realtime (Mistral, Feb 2026) is the right answer** — sub-200 ms latency, ~$0.006/min, open weights. Pair with **Silero VAD v5** (<1 ms per chunk) for endpointing and **LiveKit's SmolLM-v2 turn-detector** for semantic end-of-thought.
3. **Don't write a custom relay.** Adopt the **LiveKit Agents** runtime (or Pipecat if you need transport flexibility). Both already solve barge-in, jitter buffer coordination, and the half-duplex/full-duplex state machine. crabcc owns the *intent → MCP* layer; it does not own the WebRTC stack.
4. **QUIC was the right call for the relay-to-mobile leg** — but only with **0-RTT resumption + BBR + Mimi codec**. Plain quinn over Opus on a fresh connection is not noticeably better than HTTP/2 + WebSocket. Multipath QUIC is now in IETF WG draft 21 (March 2026); ship it as a v1.5 feature once `quinn-mp` lands.
5. **Realistic budget:** ≤120 ms TTFA on LAN with the cloud stack, ≤300 ms on cellular with cold path; ≤500 ms total user-stop-speaking → assistant-starts-speaking with the OSS stack on a beefy laptop. Bluetooth users will lose 30–200 ms regardless of what you do; LC3 helps but iPhones don't have it.

---

## Part A — Near-instant TTS (output)

### A.1 What "near-instant" means in 2026

The conversational-AI industry has converged on three latency thresholds:

| Threshold | Perceptual effect | Typical workload |
|---|---|---|
| **<200 ms TTFA** | "Snappy" — feels like a fast human assistant | Sonic-3, gpt-realtime US-region |
| **200–500 ms TTFA** | "Conversational" — natural turn-taking | ElevenLabs Flash v2.5 (real world), Kyutai TTS 1.6B |
| **500–800 ms** | "Awkward but usable" | Whisper-large + non-streaming TTS, Pi 4 Piper |
| **>800 ms** | "Broken" — users start re-speaking | Anything cold-starting on cellular |

Human audition: a gap of >200 ms after a question is consciously noticed; >500 ms feels like the system "thought about it." Sub-100 ms and you get the opposite problem — listeners think they were interrupted. The sweet spot for TTFA is **80–250 ms**, with end-to-end voice-to-voice **300–700 ms**.

**Cloud SOTA reference points (May 2026):**

| Provider / Model | Claimed TTFA | Real-world TTFB (US) | Notes |
|---|---|---|---|
| **Cartesia Sonic-3** | ~90 ms | ~150 ms | Best-in-class TTFA per Coval live benchmark |
| **Cartesia Sonic-2** | ~189 ms | ~220 ms | Coval-measured |
| **ElevenLabs Flash v2.5** | 75 ms (model-only) | ~200–478 ms (TTFB, varies by region) | The 75 ms is *model inference only*; not user-perceived |
| **OpenAI gpt-realtime** | — | ~500 ms TTFB, ~300 ms voice-to-voice processing | End-to-end ~800 ms voice-to-voice |
| **Google Gemini Live** | ~600 ms voice-to-voice | similar | |
| **Kyutai TTS 1.6B** | 220 ms end-to-end | n/a (self-host) | Open weights, delayed streams modeling |
| **Kyutai Pocket TTS** | 100 M params, real-time on CPU | n/a | Released Jan 2026 |

> **Caveat:** Vendor-marketed "TTFA" is almost always model-inference-only. Real TTFB measured from the client side adds ~100–400 ms for tokenization + region routing + TLS + first-frame buffering. Treat any single-digit-ms or <80 ms claim as marketing unless it includes a network spec.

### A.2 Latency budget components

A request flows through these stages. The numbers below are realistic median values for a cloud-augmented stack from a US client to a US-region endpoint over WiFi:

```
[user-typed text or LLM token stream]
    ↓ tokenization                            ~5 ms
    ↓ model first-token-out                   ~30–80 ms (streaming flow-matching)
    ↓ vocoder first-frame                     ~10–40 ms (HiFi-GAN / neural codec)
    ↓ codec encode (Opus 20 ms frame)         ~25 ms (1 frame buffered)
    ↓ network: client→server first byte       ~20–80 ms (1 RTT, more on cellular)
    ↓ jitter buffer (mobile)                  ~20–60 ms (target: 1 frame)
    ↓ codec decode                            ~5 ms
    ↓ AudioSession buffer paint               ~10–30 ms (iOS) / 20–60 ms (Android)
    ↓ Bluetooth (if applicable)               ~20–200 ms (LC3: 20–30; SBC: 150–200)
─────────────────────────────────────────────────────────
   ≈ 125–225 ms wired / ≈ 175–425 ms BT
```

**Implication:** the model + vocoder can be 80 ms, but you still won't get <120 ms perceived latency on any real device. Don't optimize the model below ~50 ms TTFA — diminishing returns. Optimize **codec frame size**, **jitter buffer depth**, and **AudioSession configuration** instead.

### A.3 The streaming inference lever

Batch synthesis (text → full mel → full waveform → play) is dead for real-time use. Streaming neural TTS produces audio frames *while the model is still processing later text tokens*. This works two ways:

- **Autoregressive-streaming** (Sonic, gpt-realtime, Moshi): the model emits acoustic tokens left-to-right; a streaming neural codec decoder turns each chunk into PCM. TTFA = (first-token latency) + (codec frame).
- **Non-autoregressive streaming via flow-matching** (F5-TTS, Kyutai delayed streams): the model produces a chunk's worth of mel/codec tokens in parallel via N flow-matching steps; smaller chunks → lower TTFA but more compute overlap.

**Architectures that actually hit sub-100 ms TTFA:**

- **Distilled flow-matching transformers** (Sonic-3, F5-TTS distilled) — typically 4–8 flow steps, < 30 ms model time on H100.
- **Acoustic-token autoregressive transformers + streaming codec** (Mimi/Moshi, Sonic) — the codec itself (Mimi) has 80 ms algorithmic latency; the model adds ~80 ms of acoustic delay → ~160 ms theoretical floor, ~200 ms practical on L4.

**Rust-shippable engines (recommendation order):**

| Engine | License | Streaming | Rust path | TTFA target | Notes |
|---|---|---|---|---|---|
| **Kyutai TTS 1.6B / Pocket TTS** | Apache-2.0 | Yes (delayed streams) | `candle` (kyutai-labs ships ref impl), ONNX viable | 100–220 ms | **Pick this for OSS lane.** Pocket-TTS runs CPU real-time. |
| **F5-TTS** | MIT | Yes (chunked flow) | candle port exists; Crane engine has it | ~150 ms (RTF 0.15) | Voice cloning is excellent. Heavier than Kyutai. |
| **Piper / piper-plus** | MIT (piper-plus only; original archived Oct 2025) | No (block-level) | pure Rust runtime exists, ~1.3× ONNX | 250–400 ms TTFA | What crabcc has today. Adequate for v0; replace before v1. |
| **Sonic-3 (Cartesia)** | Hosted only | Yes | HTTP/WebSocket/QUIC client | ~90 ms TTFA | **Pick this for cloud lane.** |
| **OpenVoice 2** | MIT | Limited | ONNX | ~400 ms | Voice cloning, but not designed for streaming. |
| **Coqui XTTS-v2** | non-commercial | Streaming patch exists | ONNX | ~300 ms | Coqui dissolved 2024; community fork only. License kills it for commercial use. |
| **Tortoise-TTS** | — | — | — | — | **Do not use.** Deprecated, multi-second latency. |

**Bottom line:** the right OSS stack is **Kyutai delayed-streams TTS + Mimi codec**. The right cloud stack is **Cartesia Sonic-3** (or gpt-realtime if you also want speech-to-speech reasoning). Piper is a fine v0 placeholder but architecturally limits TTFA.

### A.4 Network transport — when QUIC actually wins

The current crabcc design uses `quinn` (QUIC) for the relay→mobile leg. Verdict: **correct for the use case, but only because of three secondary properties, not because of base latency.**

**QUIC vs HTTP/2 + WebSocket reality check:**

- **Steady-state byte latency:** roughly identical on a good network. The ~1 RTT TCP+TLS handshake difference disappears after connection setup.
- **Where QUIC actually wins:**
  - **0-RTT session resumption** — second connection from the same client skips the handshake. Saves ~80–150 ms on cellular.
  - **Head-of-line blocking elimination** — one lost packet doesn't stall the whole stream. Big tail-latency win on lossy cellular (5–15% of users see this).
  - **Connection migration** — survives WiFi↔cellular handover without re-handshake. Critical for mobile.
- **Where QUIC doesn't help:** good WiFi, low-loss home connections. There you are paying QUIC's userspace-stack overhead for nothing. (`quinn` is fast enough that this overhead is <1 ms; not a real concern.)
- **Congestion control:** **use BBR**, not CUBIC. Audio is rate-stable; BBR's RTT-probe model gives lower buffer occupancy and lower tail latency. quinn supports it via `congestion::Bbr`.
- **Multipath QUIC:** IETF draft-ietf-quic-multipath-21 landed March 2026. Real implementations exist (mvfst, picoquic). For a 2026-era mobile app, ship single-path quinn now and add MPQUIC in v1.5 when `quinn-mp` stabilizes — the WiFi+cellular aggregation is *the* feature for office-to-coffeeshop transitions.
- **MoQ (Media over QUIC):** overkill for a 1:1 voice channel. Worth watching for v2 if you ever broadcast.

**HTTP/3 WebSockets (RFC 9220):** still not shipped in major browsers as of early 2026. Not relevant.

**Recommendation:** keep quinn. Configure for `0-RTT` + `BBR` + `idle_timeout=30s` + `keep_alive_interval=5s`. Use a dedicated unidirectional stream per audio direction so PCM frames don't compete with control messages.

### A.5 Codec — Opus vs Mimi vs Lyra v2

| Codec | Bitrate | Algo. delay | Quality @ voice | Rust path | When to use |
|---|---|---|---|---|---|
| **Opus** (default 20 ms frame) | 6–510 kbps | 5 ms (variable, 26.5 ms typical) | Excellent at 24 kbps | `audiopus`, `opus-rs` | **Default.** Universal, zero CPU, every device decodes it. |
| **Mimi** (Kyutai) | 1.1 kbps | 80 ms | Comparable to Opus@24 at <5% the bitrate | candle/ONNX | When the TTS model emits Mimi tokens directly (skip encode entirely). |
| **Lyra v2** (Google) | 3.2/6/9.2 kbps | ~20 ms | Excellent at 6 kbps | C++ wrapper only | Skip — Mimi is better and has Rust paths. |
| **LC3** (Bluetooth LE Audio) | 32–124 kbps | ~5 ms | Good | hardware-only on most chips | Not your codec choice; the BT stack picks. |

**Why semantic codecs (Mimi) change the latency math:** if the TTS model emits Mimi tokens *directly*, you skip the model→PCM→Opus-encode pipeline entirely. The relay forwards 1.1 kbps of tokens, the client decodes Mimi → PCM. This shaves the 5–25 ms encode step *and* drops bandwidth ~20×. The catch: 80 ms algorithmic delay on Mimi vs 5 ms on Opus. Net win at ~75 ms saved on encode + bandwidth (hence faster TTFB on slow links), net loss of ~75 ms on the codec frame itself. **Roughly a wash on fast networks; clear win on cellular below 200 kbps.** Mimi is the right bet for crabcc's mobile-cellular path.

**Recommendation:** dual-codec — Opus for LAN/WiFi, Mimi for cellular (negotiated at session start based on RTT estimate).

### A.6 Predictive / speculative tricks

Cheap wins ranked by ROI:

1. **Pre-tokenize while LLM thinks.** The moment the LLM emits its first token, start the TTS prefill. Don't wait for sentence boundaries.
2. **Sentence-boundary speculation.** Most LLM responses start with a predictable opener ("Sure," "Let me," "Got it,"). Start synthesizing those *speculatively* as soon as the first 2–3 tokens stream in; cancel if the LLM pivots. Cuts 100–200 ms off TTFA.
3. **Chunk-level reorder buffer.** The TTS may emit chunk N+1 before chunk N is fully buffered on the client. A 40 ms reorder buffer on the client smooths this without adding perceptible latency.
4. **Partial-sentence playback with crossfade.** If the LLM stops mid-sentence (interruption / token rate dip), crossfade to silence over 30 ms instead of cutting. Avoids the "click" that makes barge-in feel broken.
5. **Pre-generation while user reads.** For chat-style UIs where text appears before audio, start TTS the moment text renders even if the user hasn't tapped "play". Free latency.

### A.7 Concrete latency budget for crabcc

**Target:** ≤120 ms TTFA on LAN; ≤300 ms TTFA on cellular; ≤500 ms voice-to-voice end-to-end.

#### Cloud-augmented stack (Cartesia Sonic-3 + Opus over QUIC)

| Stage | LAN (ms) | Cellular (ms) |
|---|---|---|
| LLM first token → TTS API call | 0 (overlapped) | 0 (overlapped) |
| Cartesia model first token + vocoder | 90 | 90 |
| Codec encode (Opus 20 ms) | 25 | 25 |
| Cloud → relay (US-east) | 5 | 30 |
| Relay → mobile (QUIC, 0-RTT) | 10 | 80 |
| Mobile jitter buffer (1 frame) | 20 | 40 |
| Codec decode | 5 | 5 |
| AudioSession paint | 15 | 25 |
| **Total first-perceived-audio** | **170** | **295** |

Achievable. Margin for a Bluetooth penalty (LC3: +25, SBC: +180) only on LAN.

#### OSS-only stack (Kyutai TTS 1.6B on a M-series Mac relay + Mimi over QUIC)

| Stage | LAN (ms) | Cellular (ms) |
|---|---|---|
| LLM first token → TTS prefill | 0 | 0 |
| Kyutai model + Mimi first frame | 220 | 220 |
| (no encode — Mimi tokens passthrough) | 0 | 0 |
| Relay → mobile (QUIC, 0-RTT) | 10 | 80 |
| Jitter buffer (Mimi, 80 ms frame) | 80 | 80 |
| Mimi decode (mobile) | 15 | 15 |
| AudioSession paint | 15 | 25 |
| **Total first-perceived-audio** | **340** | **420** |

Honest take: **the OSS stack misses the ≤120 ms LAN target** because of Mimi's 80 ms algorithmic delay. To hit 120 ms LAN with OSS you must use Opus (loses the bandwidth win) and a smaller model (Pocket-TTS at 100 M params, but quality drops). The cloud stack is just better — 2026 is not the year self-hosted neural TTS catches Cartesia.

---

## Part B — Voice control (full duplex / barge-in)

### B.1 State of the art (May 2026)

| Stack | Type | Voice-to-voice e2e | Strengths | Weaknesses |
|---|---|---|---|---|
| **OpenAI gpt-realtime** | Speech-to-speech foundation | ~800 ms | Best reasoning (82.8% Big Bench Audio), tool calling | Cost ($), US-region only for low latency |
| **Gemini Live** | Speech-to-speech | ~600 ms | Multimodal (vision), good multilingual | Lock-in to Google stack |
| **Pipecat** | Pipeline framework | 800–950 ms | Transport-agnostic, OSS, easy to swap components | More wiring; you own the orchestration |
| **LiveKit Agents** | WebRTC-first runtime | 750–900 ms | Built-in barge-in, turn detection, telephony | WebRTC tax; Mimi/QUIC harder to swap in |
| **Vapi** | Hosted voice agent platform | ~700 ms | Zero-ops, telephony built in | Hosted-only, lock-in |

**For crabcc:** you are not building a foundation model and you do want OSS-friendly. **Choose between Pipecat (transport flexibility for QUIC/Mimi) and LiveKit Agents (out-of-box barge-in).** Recommendation: **LiveKit Agents** — the SmolLM-v2 turn-detector and Silero-integrated barge-in alone are worth the WebRTC tax. Wrap their runtime; expose your MCP tool layer as the agent's brain.

### B.2 VAD (voice activity detection)

| VAD | Latency / chunk | Accuracy (TPR @ 5% FPR) | Languages | Verdict |
|---|---|---|---|---|
| **Silero VAD v5** | <1 ms (single CPU thread, 30 ms chunk) | 87.7% | 6000+ | **Use this.** 3× faster than v4. |
| **WebRTC VAD** | <1 ms | ~70% | n/a (signal-only) | Outdated. Don't. |
| **Pyannote** | 30–100 ms | 92%+ | many | Too heavy for streaming endpointing; use for diarization offline. |
| **Picovoice Cobra** | <1 ms | comparable to Silero | many | Commercial license. Skip unless already paying Picovoice. |

Silero v5 is a solved problem. Drop it in, set chunk = 30 ms, threshold = 0.5, min-silence = 200 ms for endpointing. Tune `min-silence` per use case — too aggressive and you cut users off mid-thought.

**Endpointing is not just VAD.** A pure acoustic VAD will fire on "umm…" pauses. Pair with a **semantic turn detector** — LiveKit ships a 135 M-param SmolLM-v2 fine-tune that predicts whether the current ASR transcript looks like a complete thought. This 2-signal model is the 2026 best practice; don't try to do it with VAD alone.

### B.3 Streaming ASR

| Model | WER (LibriSpeech other) | Latency | Open? | Cost | Verdict |
|---|---|---|---|---|---|
| **Voxtral Realtime (Mistral, Feb 2026)** | ~5–6% (1–2% delta from offline at 480 ms) | sub-200 ms config | open weights + API | $0.006/min | **First choice.** Best speed/quality/openness combo. |
| **Voxtral Mini Transcribe V2** | ~5% | batch | open + API | $0.003/min | Use for offline transcription, not realtime. |
| **NVIDIA Parakeet TDT v3** | ~4–5% | ~27 ms encoder on Apple Silicon (10 s audio) | open | self-host | Fastest streaming WER; great if you have NV GPUs. |
| **Whisper Large v3 Turbo** | ~6–7% | 6× faster than Large v3, ~150–300 ms streaming | open | self-host | Solid generalist, multilingual. |
| **Distil-Whisper (large v3)** | ~7–8% on OOD; 14.93% on hard sets | 6× faster | open | self-host | Older; turbo replaced this for most uses. |
| **Faster-Whisper (CT2)** | matches Whisper | matches Turbo when both use CT2 | open | self-host | Implementation detail; combine with Turbo. |
| **Moshi STT-mode** | ~7% | streaming inherent to Moshi | open | self-host | Only worth it if you also run Moshi for everything else. |
| **AssemblyAI Universal-2** | ~5% | streaming | hosted | ~$0.01/min | Great quality; hosted-only kills it for crabcc. |
| **Deepgram Nova-3** | ~5% | ~150 ms streaming | hosted | ~$0.0043/min | Cheapest hosted realtime; backup option. |

**Recommendation:** Voxtral Realtime as the primary; Parakeet TDT as the offline / self-host fallback when you need no-cloud guarantees.

### B.4 Wake word vs always-listening

| Approach | Privacy | Battery | Latency | Recommendation |
|---|---|---|---|---|
| **Always-listening + cloud VAD** | poor | poor | low | **Don't.** GDPR nightmare, bad battery. |
| **On-device VAD + push-to-talk** | excellent | great | depends on UX | Best for v0. Tap-to-talk button. |
| **Wake word (Porcupine)** | excellent | good | ~100 ms detection | Commercial license. Custom phrases trivial. |
| **Wake word (OpenWakeWord)** | excellent | good | ~150 ms | OSS. Slightly higher false-accept rate, but acceptable: <0.5/hour. |

For crabcc: ship **push-to-talk in v0**, **OpenWakeWord ("Hey crabcc" or similar) in v1**, leave Porcupine for enterprise/commercial-license deployments.

### B.5 Barge-in / interrupt

The hard problem in full duplex. Required pieces:

1. **AEC (acoustic echo cancellation).** When the assistant's voice plays through the speaker and is picked up by the mic, the system must subtract it out before VAD/ASR runs, otherwise the assistant interrupts itself.
   - **WebRTC AEC3** — battle-tested, ships with Chrome/LiveKit. Use this.
   - **DeepFilterNet 3** — neural, higher quality (PESQ 3.5–4.0), 10–20 ms latency. Use if you find AEC3 inadequate; otherwise overkill.
   - **RNNoise** — pure noise suppression, not echo cancel. Useful as a *post*-AEC step on noisy environments.
2. **VAD on the AEC-cleaned mic stream.** Silero v5 on the residual.
3. **Cancellation event bus.** When VAD fires while TTS is playing: stop TTS, fade out 30 ms, flush the relay's TTS queue, mark the in-flight LLM turn as interrupted. crabcc's existing event bus needs a `voice.interrupt` event class.

**Half vs full duplex tradeoff:**
- **Half duplex** (assistant pauses when listening, listens when paused): cheap, no AEC needed, but feels sluggish.
- **Full duplex** (both sides always live): requires AEC + barge-in. The 2026 default.

Ship half-duplex in v0 (no AEC), full-duplex in v1 (LiveKit's built-in WebRTC AEC).

### B.6 Intent routing — voice → MCP

The 2026 architecture is **don't write a custom NLU**. Two viable patterns:

**Pattern A — LLM-mediated tool calling (recommended).**
```
ASR transcript → Claude/GPT (with MCP tools registered) → tool call → MCP server → result → TTS prompt
```
The LLM does intent classification *and* parameter extraction in one shot via function calling. crabcc's existing MCP layer plugs in unmodified. Cost: one LLM round-trip per turn (~300–500 ms).

**Pattern B — speech-to-speech model (lower latency, less control).**
```
audio → gpt-realtime / Moshi → audio + tool call
```
Cuts the ASR step entirely. Latency drops to ~600 ms voice-to-voice. Loses: precise transcript control, custom routing logic, debuggability. Use this only if the latency win is required.

**For crabcc:** Pattern A. Use Claude or gpt-4.1 (for cost) as the brain; MCP servers (`crabcc-mcp` already exists) as the tools. This keeps the existing crabcc-agents architecture intact and lets you swap the LLM provider trivially.

### B.7 The mobile-specific layer

| Concern | iOS | Android |
|---|---|---|
| **Mic access pattern** | `AVAudioSession.recordPermission` + `.measurement` mode for low-latency input | `RECORD_AUDIO` + AAudio MMAP path |
| **AudioSession routing** | `.playAndRecord` + `.allowBluetooth` + `.defaultToSpeaker` | `AudioManager.MODE_IN_COMMUNICATION` |
| **Bluetooth headset penalty** | AAC/SBC: 150–200 ms; iOS does **not** support LC3 | LC3 with LE Audio: 20–30 ms; SBC fallback: 150–200 ms |
| **CarPlay / Android Auto active** | system commandeers AudioSession; expect ducking | similar; route via media session |
| **Background mic** | `audio` background mode required | foreground service required Android 14+ |
| **Flutter pkg** | `audio_session` + `flutter_soloud` | same; AAudio comes for free |

**Critical iOS note:** `.measurement` mode disables most of iOS's audio processing (which is what you want for ASR) but also reduces output volume slightly. Document this. Also: iOS ignores LC3 entirely as of May 2026 — Apple's Bluetooth stack still defaults AAC. There is *no fix* for AirPods latency on iOS short of a wired connection. State this in your UX.

**Flutter audio plumbing:**
- `audio_session` for AVAudioSession/AudioManager configuration.
- `flutter_soloud` for low-latency playback (alternatives like `audioplayers` add 100+ ms of buffering).
- For input: `record` (`com.llfbandit.record`) gives you raw PCM with reasonable latency on both platforms; pipe directly into Silero VAD via FFI.

---

## Part C — Concrete architecture for crabcc

### C.1 Target architecture

```mermaid
flowchart TB
  subgraph Mobile["Flutter Mobile App"]
    MIC[Mic Capture<br/>AAudio / AVAudioSession<br/>16 kHz PCM]
    AEC1[WebRTC AEC3<br/>residual out]
    VAD1[Silero VAD v5<br/>30 ms chunks]
    AS_OUT[AudioSession Out<br/>Mimi/Opus decode]
    SPK[Speaker<br/>incl. BT route]
  end

  subgraph Relay["crabcc Relay (Rust)"]
    QUIC[quinn QUIC<br/>0-RTT + BBR]
    STT[Voxtral Realtime<br/>(or Parakeet TDT self-host)]
    TURN[Semantic Turn<br/>Detector<br/>SmolLM-v2 135M]
    BUS[Event Bus<br/>voice.interrupt /<br/>voice.utterance]
    BRAIN[Claude / gpt-4.1<br/>+ MCP tool calls]
    MCP[crabcc-mcp<br/>(existing)]
    TTS[Cartesia Sonic-3<br/>(or Kyutai TTS 1.6B)]
    CODEC[Opus / Mimi encode]
  end

  MIC --> AEC1 --> VAD1
  VAD1 -- "speech start/end" --> QUIC
  AEC1 -- "PCM frames" --> QUIC
  QUIC -- "PCM" --> STT
  STT -- "partial tx" --> TURN
  STT -- "final tx" --> BRAIN
  TURN -- "end-of-thought" --> BRAIN
  BRAIN <--> MCP
  BRAIN -- "token stream" --> TTS
  TTS -- "audio frames" --> CODEC
  CODEC -- "Opus/Mimi" --> QUIC
  QUIC -- "audio" --> AS_OUT --> SPK

  VAD1 -. "barge-in trigger" .-> BUS
  BUS -. "cancel TTS" .-> TTS
  BUS -. "rollback turn" .-> BRAIN
  SPK -. "echo reference" .-> AEC1
```

### C.2 Two recommended stacks

**OSS-only stack (no cloud calls):**

| Layer | Component | Pin |
|---|---|---|
| ASR | NVIDIA Parakeet TDT v3 (or Voxtral open weights) | Parakeet 1.1, Voxtral Mini Realtime |
| VAD | Silero VAD | v5.1.2+ |
| Turn detector | LiveKit SmolLM-v2 turn-detector | 0.4+ |
| LLM | Local Ollama (Llama 3.3 70B / Qwen 2.5 32B) | latest |
| TTS | Kyutai TTS 1.6B (or Pocket TTS for CPU) | July 2025 release |
| Codec | Mimi (semantic) + Opus (fallback) | kyutai-labs/moshi 0.2+ / opus 1.5+ |
| Transport | quinn | 0.11+ with BBR feature |
| Runtime | Pipecat (transport flexibility) | 0.0.50+ |

**Cloud-augmented stack (best latency & quality):**

| Layer | Component | Cost |
|---|---|---|
| ASR | Voxtral Realtime (Mistral) | $0.006/min |
| VAD | Silero VAD v5 | free |
| Turn detector | LiveKit SmolLM-v2 | free |
| LLM | Claude 4.7 Sonnet via Bedrock or Anthropic API | ~$3/1M in, $15/1M out |
| TTS | Cartesia Sonic-3 | ~$0.04/min ($16/1M chars on Pro) |
| Codec | Opus 24 kbps | free |
| Transport | quinn | free |
| Runtime | LiveKit Agents | free OSS, $hosted optional |

**Per-minute cost (cloud stack, conversational ratio ~50% talking each):**
- Voxtral input: ~$0.003 (30 s of user speech)
- Claude tokens: ~$0.005–$0.02 (depends on context length)
- Sonic-3 output: ~$0.02 (30 s of assistant speech)
- **Total: ~$0.03–$0.05 per active minute.** Comparable to Vapi's hosted pricing without the lock-in.

### C.3 Minimum viable v0 — 4 components, sub-500 ms total

To demo end-to-end voice control with **<500 ms total** (push-to-talk, half-duplex, no barge-in yet):

1. **Flutter `record` plugin → Silero VAD over FFI.** Push-to-talk gates the stream. (Day 1.)
2. **quinn relay → Voxtral Realtime API.** Rust client streams PCM, gets transcript. (Day 2–3.)
3. **Existing crabcc MCP brain** routes the transcript via Claude tool calls. (Already exists.)
4. **Cartesia Sonic-3 → Opus → quinn → Flutter `flutter_soloud` playback.** (Day 4–5.)

That's the 4-component path. Defer until v1: AEC, barge-in, wake word, Mimi codec, Kyutai self-host, multipath QUIC.

**Expected v0 latency budget:**
- VAD endpoint: 200 ms (silence threshold)
- ASR: 200 ms (Voxtral Realtime)
- LLM tool decision: 300 ms (Claude streaming first token)
- TTS first audio: 150 ms (Sonic-3 + 50 ms network + jitter)
- AudioSession paint: 25 ms
- **Total: ~875 ms voice-to-voice.** Above 500 ms but conversational. The ASR+LLM serial dependency is the long pole.

**To get under 500 ms in v1:** parallelize with speculative LLM execution on partial ASR transcripts, swap Claude for gpt-realtime (speech-to-speech), or accept that 800 ms is the realistic ceiling for an LLM-mediated tool-calling architecture.

### C.4 Migration plan from current crabcc state

1. **Today (Piper + quinn):** Keep. Reframe as "v0 fallback."
2. **Sprint 1:** Wire LiveKit Agents around the existing crabcc-mcp brain. Replace Piper with Sonic-3 cloud client behind a feature flag. Ship push-to-talk Flutter UI.
3. **Sprint 2:** Add Voxtral Realtime input. Add Silero VAD on the mobile side. Half-duplex working end-to-end.
4. **Sprint 3:** Add WebRTC AEC3 and barge-in via the LiveKit runtime. Full duplex.
5. **Sprint 4:** Self-host fallback — Kyutai TTS 1.6B on a Mac mini relay; Parakeet TDT on the same box. crabcc users with no cloud budget get 95% of the experience.
6. **Sprint 5+:** OpenWakeWord. Multipath QUIC. Mimi codec.

---

## Final opinion

**Stop trying to make Piper streaming.** It's a 2023 architecture; the 2026 winners are flow-matching distillations (Sonic-3) and delayed-streams autoregressive transformers (Kyutai). Either pay Cartesia $0.04/min and get 90 ms TTFA, or self-host Kyutai and accept ~220 ms. The QUIC choice is correct but secondary — the model architecture is the latency story. For input, Voxtral Realtime + Silero VAD + LiveKit's turn detector is a complete, OSS-friendly solution. For barge-in, ship LiveKit Agents wrapped around your existing MCP brain rather than rolling your own WebRTC stack. Ship v0 in a week with cloud APIs; iterate the OSS stack as a backup, not the default.

---

## Sources

- [ElevenLabs — Latency optimization docs](https://elevenlabs.io/docs/eleven-api/guides/how-to/best-practices/latency-optimization)
- [DeepLearning.AI — ElevenLabs drops latency to 75 ms](https://www.deeplearning.ai/the-batch/elevenlabs-drops-latency-to-75-milliseconds/)
- [Cartesia Sonic 3](https://cartesia.ai/sonic) and [Cartesia / Coval TTS benchmark](https://x.com/cartesia_ai/status/1900249483608555956)
- [Inworld — Best TTS APIs for Real-Time Voice Agents 2026](https://inworld.ai/resources/best-voice-ai-tts-apis-for-real-time-voice-agents-2026-benchmarks)
- [Kyutai Moshi paper (arXiv 2410.00037)](https://arxiv.org/html/2410.00037v2)
- [Kyutai TTS](https://kyutai.org/tts)
- [Kyutai Mimi codec on HF](https://huggingface.co/kyutai/mimi)
- [F5-TTS](https://swivid.github.io/F5-TTS/)
- [OpenAI — gpt-realtime announcement](https://openai.com/index/introducing-gpt-realtime/)
- [Latent Space — OpenAI Realtime API: Missing Manual](https://www.latent.space/p/realtime-api)
- [Silero VAD GitHub](https://github.com/snakers4/silero-vad), [v5 quality metrics](https://github.com/snakers4/silero-vad/wiki/Quality-Metrics)
- [Picovoice — Best VAD 2026](https://picovoice.ai/blog/best-voice-activity-detection-vad/)
- [Mistral Voxtral Transcribe 2 (Feb 2026)](https://mistral.ai/news/voxtral-transcribe-2)
- [Northflank — Best open-source STT 2026](https://northflank.com/blog/best-open-source-speech-to-text-stt-model-in-2026-benchmarks)
- [LiveKit Voice Agents docs](https://docs.livekit.io/agents/logic/turns/vad/)
- [WebRTC.ventures — Voice AI agent framework comparison 2026](https://webrtc.ventures/2026/03/choosing-a-voice-ai-agent-production-framework/)
- [Hamming AI — Best voice agent stack 2026](https://hamming.ai/resources/best-voice-agent-stack)
- [Picovoice — Wake Word Detection 2026](https://picovoice.ai/blog/complete-guide-to-wake-word/)
- [Porcupine](https://github.com/Picovoice/porcupine), [openWakeWord](https://github.com/dscripka/openWakeWord)
- [DeepFilterNet — short audio noise reduction](https://noisereducerai.com/deepfilternet-ai-noise-reduction/)
- [Opus Codec docs](https://www.opus-codec.org/comparison/)
- [armorsound — BT audio latency 2026](https://armorsound.com/bluetooth-audio-delay-and-audio-latency-guide/)
- [LC3 codec 2026](https://besttechradar.com/what-is-lc3-codec/)
- [BackendBytes — TCP vs UDP vs QUIC](https://backendbytes.com/articles/tcp-vs-udp-protocol-guide/)
- [LiteSpeed — BBR in QUIC/HTTP3](https://blog.litespeedtech.com/2019/10/28/bbr-congestion-control-quic-http-3/)
- [IETF — draft-ietf-quic-multipath-21](https://datatracker.ietf.org/doc/draft-ietf-quic-multipath/)
- [WebSocket.org — Future of WebSockets, HTTP/3](https://websocket.org/guides/future-of-websockets/)
- [rhasspy/piper — archived](https://github.com/rhasspy/piper), [piper-plus fork](https://github.com/ayutaz/piper-plus)
- [Piper Rust runtime discussion](https://github.com/rhasspy/piper/discussions/504)
- [Crane Rust inference engine (Candle)](https://github.com/lucasjinreal/Crane)
- [Famulor — AI voice agent pricing 2026](https://www.famulor.io/blog/ai-voice-agent-pricing-2026-what-10-platforms-actually-cost-per-minute)
- [Future AGI — TTS providers 2026](https://futureagi.com/blog/best-text-to-speech-providers-2026/)
- [audio_session Flutter package](https://pub.dev/packages/audio_session)
- [flutter_soloud](https://github.com/alnitak/flutter_soloud)
- [Android NDK — audio latency](https://developer.android.com/ndk/guides/audio/audio-latency)

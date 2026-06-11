# Zero-allocation stdio: how we squeezed 18% more throughput from crabcc-mcp
## (by stopping the UTF-8 double-check)

**Draft — June 2026**

---

crabcc speaks JSON-RPC over stdio. When an AI agent calls `tools/call`, the request arrives as a line of JSON on stdin. The MCP server parses it, dispatches the tool, and writes the response back on stdout. Millions of times per session.

The obvious implementation — the one every MCP tutorial shows — looks like this:

```rust
let mut line = String::new();
while reader.read_line(&mut line)? > 0 {
    let req: Value = serde_json::from_str(&line)?;
    let resp = handle(&req);
    writeln!(writer, "{}", resp)?;
    line.clear();
}
```

Three lines. Clean, correct, and slower than it needs to be. Here's why.

### The hidden work in `read_line`

`read_line` doesn't just read bytes until `\n`. It also validates UTF-8 on every byte as it reads. It has to — it returns a `String`, which is guaranteed valid UTF-8.

The problem: serde_json's parser also validates UTF-8 on the strings it cares about. So every byte of incoming JSON gets UTF-8 checked *twice*: once by `read_line` building the String, and once by `serde_json` parsing the String. That's wasted CPU.

### The hidden allocation in `writeln!`

`writeln!(writer, "{}", resp)` calls `Value::to_string()`, which allocates a `String` to hold the serialized response. Then it writes that String to the output buffer. Two copies of the response data in memory: the String, and the writer's buffer.

### The fix: two small changes

**1. `read_until` + `from_slice`**

```rust
let mut buf: Vec<u8> = Vec::with_capacity(4096);
loop {
    buf.clear();
    reader.read_until(b'\n', &mut buf)?;
    let req: Value = serde_json::from_slice(&buf)?;
    let resp = handle(&req);
    serde_json::to_writer(&mut writer, &resp)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
}
```

`read_until` reads raw bytes — no UTF-8 validation. `from_slice` parses JSON from a `&[u8]`, doing its own UTF-8 checks only on the string values it needs. One validation instead of two.

**2. `to_writer` instead of `to_string` + write**

`to_writer` serializes directly into the output buffer. No intermediate String. The JSON bytes land exactly once in memory.

**3. Reuse the buffer**

The `Vec<u8>` gets `.clear()`'d each iteration but keeps its capacity. After the first large request (a full symbol lookup might return 50KB of JSON), the buffer sizes up. Subsequent requests reuse that capacity — no reallocation.

### What we measured

On a synthetic agent workload (200 mixed `sym`/`refs`/`callers`/`outline` calls over a 13,000-file Rust monorepo):

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Throughput (req/s) | 1,420 | 1,740 | +22.5% |
| Allocations per request | 4.2 | 1.0 | -76% |
| Peak memory (KB) | 680 | 420 | -38% |

The allocation drop is the real story. Every `read_line` call allocated a new String (the old one was `.clear()`'d but the buffer was dropped and reallocated on each iteration if it grew). Every `writeln!` allocated a String for the serialized response. The new path has one allocation: the `Vec<u8>` buffer, which grows once and stabilizes.

### First-request vs steady-state

The first request typically reads a large response (tools/list returns every tool schema). The buffer grows to accommodate it — say 16KB. Every subsequent request sees zero allocations because 16KB covers the common case. Most MCP request/response pairs fit in a single TCP segment (1,460 bytes).

### The principle

`read_line` is for when you need a `String`. If you're about to parse that String as JSON, you don't — you need bytes. Let the parser do the UTF-8 work. `to_string` is for when you need a String. If you're about to write it to a buffer, you don't — serialize directly.

**Lesson:** The most obvious Rust API isn't always the fastest. Sometimes the "lower-level" alternative (`read_until`, `from_slice`, `to_writer`) gives you exactly what you need with less hidden work.

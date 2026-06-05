from time import time as _time

def now() -> float:
    return _time()

# ---- chunking ----
def chunk_text(text: str, size: int, overlap: int) -> list[str]:
    chunks = list[str]()
    i = 0
    while i < len(text):
        chunks.append(text[i:i+size])
        i += size - overlap
    return chunks

# ---- compression: drop stopwords + deduplicate sentences ----
STOPWORDS = {
    "the","a","an","is","are","was","were","it","in","on","at","to","of",
    "and","or","but","for","with","this","that","be","as","by","from",
    "not","have","has","had","they","we","you","he","she","do","did",
}

def compress_chunk(chunk: str, ratio: float) -> str:
    words = chunk.split()
    kept = list[str]()
    for w in words:
        if w.lower().rstrip(".,!?;:") not in STOPWORDS:
            kept.append(w)
    target = int(len(kept) * ratio)
    return " ".join(kept[:max(1, target)])

def pipeline(text: str, chunk_size: int, overlap: int, ratio: float) -> list[str]:
    chunks = chunk_text(text, chunk_size, overlap)
    return [compress_chunk(c, ratio) for c in chunks]

# ---- generate synthetic corpus ----
def make_corpus(n: int) -> str:
    sentence = ("The agent processed the document and extracted the relevant information "
                "from the context window using the tool and returned the result to the caller. ")
    result = ""
    for _ in range(n):
        result += sentence
    return result

corpus = make_corpus(2000)   # ~200 KB
t0 = now()
results = pipeline(corpus, chunk_size=500, overlap=50, ratio=0.6)
elapsed = now() - t0

total_in  = len(corpus)
total_out = sum(len(r) for r in results)
print(f"chunks:     {len(results)}")
print(f"input:      {total_in} chars")
print(f"output:     {total_out} chars")
print(f"ratio:      {total_out * 100.0 / total_in:.2f}%")
print(f"time:       {elapsed * 1000.0:.1f} ms")

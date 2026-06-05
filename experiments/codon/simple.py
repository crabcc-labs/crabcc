# Simple: string ops, list comprehension, dict manipulation
def tokenize(text: str) -> list[str]:
    return text.lower().split()

def count_tokens(text: str) -> dict[str, int]:
    counts = dict[str, int]()
    for tok in tokenize(text):
        counts[tok] = counts.get(tok, 0) + 1
    return counts

def top_n(counts: dict[str, int], n: int) -> list[tuple[str, int]]:
    items = list(counts.items())
    items.sort(key=lambda x: -x[1])
    return items[:n]

text = "the agent called the tool and the tool returned the result and the agent used the result"
counts = count_tokens(text)
for word, freq in top_n(counts, 5):
    print(f"{word}: {freq}")

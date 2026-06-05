# Agent-loop-shaped: structured output parsing, branching, budget tracking
from typing import Optional

class Budget:
    def __init__(self, limit: int):
        self.limit = limit
        self.spent = 0

    def consume(self, tokens: int) -> bool:
        if self.spent + tokens > self.limit:
            return False
        self.spent += tokens
        return True

    def remaining(self) -> int:
        return self.limit - self.spent

def estimate_tokens(text: str) -> int:
    return len(text) // 4

def compress_if_needed(text: str, budget: Budget, ratio: float = 0.5) -> str:
    est = estimate_tokens(text)
    if budget.consume(est):
        return text
    # over budget: naive truncation as fallback
    target_chars = int(len(text) * ratio)
    return text[:target_chars] + "..."

def route(intent: str) -> str:
    intent = intent.lower()
    if "search" in intent:
        return "search_tool"
    elif "write" in intent or "edit" in intent:
        return "edit_tool"
    elif "run" in intent or "exec" in intent:
        return "bash_tool"
    else:
        return "ask_user"

def agent_turn(messages: list[str], budget: Budget) -> list[str]:
    results = list[str]()
    for msg in messages:
        compressed = compress_if_needed(msg, budget)
        tool = route(compressed)
        results.append(f"[{tool}] {compressed[:60]}")
        if budget.remaining() < 100:
            results.append("(budget low — stopping early)")
            break
    return results

budget = Budget(500)
messages = [
    "Please search for the latest Codon benchmarks on single-threaded performance.",
    "Write a summary of the results to /tmp/codon-summary.md.",
    "Run the codon compile step and report any errors.",
    "Search for Ray Serve documentation on batched inference.",
    "Edit the compress.py file to add a cache layer.",
]

for line in agent_turn(messages, budget):
    print(line)
print(f"\ntokens spent: {budget.spent}/{budget.limit}")

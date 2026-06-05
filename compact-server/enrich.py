from __future__ import annotations
import functools

_MODEL_ID = "Qwen/Qwen2.5-14B-Instruct-4bit"
_MAX_TOKENS = 512

SYSTEM_PROMPT = """You are a senior software engineer acting as a Planner.
You receive:
1. COMPRESSED CONTEXT — relevant code, compressed for brevity.
2. TASK — the engineer's question or objective.

Output a tight Markdown checklist (max 8 items) mapping the task to specific
locations in the context. Be concrete: name files, functions, line patterns.
No prose. Just the checklist."""

@functools.lru_cache(maxsize=1)
def _model_and_tokenizer():
    from mlx_lm import load
    return load(_MODEL_ID)

def enrich(text: str, query: str) -> str:
    model, tokenizer = _model_and_tokenizer()
    from mlx_lm import generate

    prompt = (
        f"COMPRESSED CONTEXT:\n```\n{text[:8000]}\n```\n\n"
        f"TASK: {query}\n\n"
        "Write the attack plan checklist:"
    )
    messages = [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": prompt},
    ]
    formatted = tokenizer.apply_chat_template(
        messages, tokenize=False, add_generation_prompt=True
    )
    response = generate(model, tokenizer, prompt=formatted, max_tokens=_MAX_TOKENS)
    return response.strip()

def loaded_models() -> list[str]:
    if _model_and_tokenizer.cache_info().currsize > 0:
        return [_MODEL_ID]
    return []

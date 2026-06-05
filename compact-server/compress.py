from __future__ import annotations
import functools

_MODEL_NAME = "microsoft/llmlingua-2-xlm-roberta-large-meetingbank"

@functools.lru_cache(maxsize=1)
def _compressor():
    from llmlingua import PromptCompressor
    return PromptCompressor(
        model_name=_MODEL_NAME,
        use_llmlingua2=True,
        device_map="mps",
    )

def compact(text: str, ratio: float = 0.5) -> dict:
    compressor = _compressor()
    result = compressor.compress_prompt(
        text,
        rate=ratio,
        force_tokens=["\n"],
        chunk_end_tokens=[".", "!", "?", "\n"],
    )
    original_tokens = _estimate_tokens(text)
    compressed_tokens = _estimate_tokens(result["compressed_prompt"])
    return {
        "compressed": result["compressed_prompt"],
        "original_tokens": original_tokens,
        "compressed_tokens": compressed_tokens,
    }

def loaded_models() -> list[str]:
    if _compressor.cache_info().currsize > 0:
        return [_MODEL_NAME]
    return []

def _estimate_tokens(text: str) -> int:
    return max(1, len(text) // 4)

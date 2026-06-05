import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))

def test_compact_returns_required_keys():
    from compress import compact
    result = compact("hello world " * 600, ratio=0.5)
    assert set(result.keys()) == {"compressed", "original_tokens", "compressed_tokens"}

def test_compact_reduces_token_count():
    from compress import compact
    text = "def handle_request(req):\n    return req.body()\n" * 40
    result = compact(text, ratio=0.5)
    assert result["compressed_tokens"] < result["original_tokens"]
    assert len(result["compressed"]) > 0

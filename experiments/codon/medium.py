# Medium: agent loop scaffolding — tool dispatch, retry, state accumulation
from typing import Optional

def parse_tool_call(raw: str) -> tuple[str, str]:
    # naive: "tool_name:arg"
    parts = raw.split(":", 1)
    if len(parts) == 2:
        return parts[0].strip(), parts[1].strip()
    return parts[0].strip(), ""

def dispatch(tool: str, arg: str) -> Optional[str]:
    if tool == "search":
        return f"[result for '{arg}']"
    elif tool == "read":
        return f"[contents of '{arg}']"
    elif tool == "summarize":
        return f"[summary: {arg[:20]}...]"
    else:
        return None

def run_agent(steps: list[str], max_retries: int = 2) -> list[str]:
    history = list[str]()
    for step in steps:
        tool, arg = parse_tool_call(step)
        result = None
        for attempt in range(max_retries + 1):
            result = dispatch(tool, arg)
            if result is not None:
                break
        if result is None:
            history.append(f"FAIL: unknown tool '{tool}'")
        else:
            history.append(f"{tool}({arg}) -> {result}")
    return history

plan = [
    "search: codon performance benchmarks",
    "read: /tmp/notes.txt",
    "summarize: Codon compiles Python to native machine code with 10-100x speedups",
    "unknown_tool: this should fail",
]

for entry in run_agent(plan):
    print(entry)

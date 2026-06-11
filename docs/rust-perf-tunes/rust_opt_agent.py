"""Two-pass Rust optimization agent driven by the RustPerformanceTune schema.

Pass 1 (scanner): cheap regex/AST trigger match to flag which tunes a file may
violate. Pass 2 (refactorer): feed the file plus ONLY the matching tune rules
into a coding agent, avoiding context dilution from dumping the whole dataset.

See README.md for the full workflow and CI/CD blueprint.
"""

import json
import re
from openai import OpenAI


class RustOptAgent:
    def __init__(self, tunes_db_path: str):
        with open(tunes_db_path, "r") as f:
            self.tunes = json.load(f)
        self.client = OpenAI()  # Assuming OPENAI_API_KEY is in environment

    def scan_and_identify_tunes(self, file_content: str):
        """Scans code using regex trigger rules to find applicable tunes."""
        applicable_tunes = []
        for tune in self.tunes:
            regex_str = tune["trigger_condition"].get("anti_pattern_regex")
            if regex_str:
                # Compile regex safely
                if re.search(regex_str, file_content):
                    applicable_tunes.append(tune)
        return applicable_tunes

    def optimize_file(self, file_path: str):
        """Dispatches matching tunes directly into an agent optimization loop."""
        with open(file_path, "r") as f:
            original_code = f.read()

        matched_tunes = self.scan_and_identify_tunes(original_code)

        if not matched_tunes:
            print(f"🎉 Codebase looks highly optimized for {file_path}!")
            return

        print(f"🤖 Found {len(matched_tunes)} optimization vectors. Invoking Refactor Agent...")

        # Construct highly contextual system prompt containing only the explicit rules needed
        system_prompt = (
            "You are an elite automated Rust refactoring agent. Your task is to apply the provided "
            "optimization rules ('tunes') to the provided source code. Do not break public APIs or "
            "logic functionality. Output only valid code inside a raw code block.\n\n"
            f"RULES TO ENFORCE:\n{json.dumps(matched_tunes, indent=2)}"
        )

        user_content = f"Please optimize this source code:\n\n```rust\n{original_code}\n```"

        response = self.client.chat.completions.create(
            model="gpt-4o",  # Or claude-3-5-sonnet
            messages=[
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_content},
            ],
            temperature=0.1,  # Keep it deterministic
        )

        optimized_code = response.choices[0].message.content

        # In production, route this straight to `cargo check` & `cargo test`
        # to verify agent didn't introduce a compiler error.
        print("🚀 Refactor Complete! Verify with `cargo test` before committing.")
        return optimized_code


# Example Usage:
# agent = RustOptAgent("docs/rust-perf-tunes/tunes.example.json")
# optimized_output = agent.optimize_file("src/node.rs")

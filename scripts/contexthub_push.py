#!/usr/bin/env python3
"""Push crabcc agent context files to the LangSmith Context Hub.

Reads LANGSMITH_API_KEY and LANGSMITH_ENDPOINT from the environment.
EU tenant: set LANGSMITH_ENDPOINT=https://eu.api.smith.langchain.com

Usage:
    python scripts/contexthub_push.py [--env staging|production] [--dry-run]

Requirements:
    pip install langsmith>=0.7.35
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent.resolve()

# Files that propagate to agent runtimes. Keys are paths relative to
# REPO_ROOT; values are the logical names stored in the Context Hub entry.
CONTEXT_FILES = {
    "AGENTS.md": "AGENTS.md",
    "CLAUDE.md": "CLAUDE.md",
    "skill/crabcc/SKILL.md": "skill/crabcc/SKILL.md",
    "commands/crabcc-init.md": "commands/crabcc-init.md",
    "commands/crabcc-upgrade.md": "commands/crabcc-upgrade.md",
}

AGENT_IDENTIFIER = "crabcc"
AGENT_DESCRIPTION = (
    "crabcc — symbol-aware code indexer + agent orchestration"
)


def get_commit_hash() -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def read_files() -> dict:
    """Read each context file and return a name -> raw content mapping.

    No langsmith import needed; safe to call for --dry-run.
    """
    contents = {}
    for rel_path, hub_name in CONTEXT_FILES.items():
        abs_path = REPO_ROOT / rel_path
        if not abs_path.exists():
            print(
                f"WARNING: {rel_path} not found — skipping",
                file=sys.stderr,
            )
            continue
        contents[hub_name] = abs_path.read_text(encoding="utf-8")
    return contents


def build_file_entries(contents: dict) -> dict:
    """Wrap raw content strings in FileEntry objects (requires langsmith)."""
    from langsmith.schemas import FileEntry

    return {name: FileEntry(content=text) for name, text in contents.items()}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Push crabcc context files to LangSmith Context Hub."
    )
    parser.add_argument(
        "--env",
        choices=["staging", "production"],
        default="staging",
        help="Destination environment tag (default: staging).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print what would be pushed without making any API call.",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()

    api_key = os.environ.get("LANGSMITH_API_KEY")
    endpoint = os.environ.get(
        "LANGSMITH_ENDPOINT", "https://eu.api.smith.langchain.com"
    )

    if not api_key and not args.dry_run:
        print(
            "ERROR: LANGSMITH_API_KEY is not set. "
            "Export the key or use --dry-run.",
            file=sys.stderr,
        )
        sys.exit(1)

    commit = get_commit_hash()
    contents = read_files()

    if not contents:
        print("ERROR: No context files could be read. Aborting.", file=sys.stderr)
        sys.exit(1)

    print(f"Context Hub push — identifier: {AGENT_IDENTIFIER!r}")
    print(f"  endpoint  : {endpoint}")
    print(f"  env tag   : {args.env}")
    print(f"  commit    : {commit}")
    print(f"  files ({len(contents)}):")
    for name, text in contents.items():
        print(f"    {name}  ({len(text):,} bytes)")

    if args.dry_run:
        print("\n[dry-run] No API call made.")
        return

    from langsmith import Client

    files = build_file_entries(contents)
    client = Client(api_url=endpoint, api_key=api_key)

    url = client.push_agent(
        AGENT_IDENTIFIER,
        files=files,
        description=AGENT_DESCRIPTION,
        tags=[args.env],
    )

    print(f"\nPushed successfully.")
    print(f"  commit : {commit}")
    print(f"  url    : {url}")


if __name__ == "__main__":
    main()

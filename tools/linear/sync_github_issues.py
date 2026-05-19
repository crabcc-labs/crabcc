#!/usr/bin/env python3
"""Backfill open GitHub issues from peterlodri-sec/crabcc into Linear (crabcc project).

Requires: gh CLI, LINEAR_API_KEY (Linear → Settings → API → Personal API keys).

With GitHub↔Linear sync enabled, new issues sync automatically; run this only for
one-time backfill or after changing label mapping.

Usage:
  export LINEAR_API_KEY=lin_api_...
  python3 tools/linear/sync_github_issues.py
  python3 tools/linear/sync_github_issues.py --dry-run

Idempotent: skips issues whose Linear title already starts with GH-<n>:
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

REPO = "peterlodri-sec/crabcc"
TEAM_KEY = "VIB"
PROJECT_NAME = "crabcc"
LINEAR_API = "https://api.linear.app/graphql"


def gh_open_issues() -> list[dict]:
    out = subprocess.check_output(
        [
            "gh",
            "issue",
            "list",
            "--repo",
            REPO,
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,title,state,labels,url,body",
        ],
        text=True,
    )
    return json.loads(out)


def linear_graphql(query: str, variables: dict | None = None) -> dict:
    key = os.environ.get("LINEAR_API_KEY")
    if not key:
        sys.exit("LINEAR_API_KEY is not set")
    payload = json.dumps({"query": query, "variables": variables or {}}).encode()
    req = urllib.request.Request(
        LINEAR_API,
        data=payload,
        headers={"Authorization": key, "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        data = json.loads(resp.read())
    if data.get("errors"):
        raise RuntimeError(json.dumps(data["errors"], indent=2))
    return data["data"]


def find_team_id() -> str:
    data = linear_graphql("query { teams { nodes { id key name } } }")
    for t in data["teams"]["nodes"]:
        if t["key"] == TEAM_KEY:
            return t["id"]
    raise SystemExit(f"team {TEAM_KEY!r} not found")


def find_project_id() -> str:
    data = linear_graphql(
        """
        query($filter: ProjectFilter) {
          projects(filter: $filter, first: 10) { nodes { id name } }
        }
        """,
        {"filter": {"name": {"eq": PROJECT_NAME}}},
    )
    nodes = data["projects"]["nodes"]
    if not nodes:
        raise SystemExit(f"project {PROJECT_NAME!r} not found — create it in Linear first")
    return nodes[0]["id"]


def label_ids_by_name(team_id: str) -> dict[str, str]:
    data = linear_graphql(
        """
        query($teamId: String!) {
          issueLabels(filter: { team: { id: { eq: $teamId } } }, first: 100) {
            nodes { id name }
          }
        }
        """,
        {"teamId": team_id},
    )
    return {n["name"]: n["id"] for n in data["issueLabels"]["nodes"]}


def existing_gh_numbers(team_id: str) -> set[int]:
    data = linear_graphql(
        """
        query($teamId: ID!) {
          team(id: $teamId) {
            issues(filter: { title: { contains: "GH-" } }, first: 250) {
              nodes { title }
            }
          }
        }
        """,
        {"teamId": team_id},
    )
    nums: set[int] = set()
    for node in data["team"]["issues"]["nodes"]:
        title = node["title"]
        if title.startswith("GH-"):
            try:
                nums.add(int(title.split(":", 1)[0][3:]))
            except ValueError:
                pass
    return nums


def map_labels(gh_labels: list[dict]) -> tuple[list[str], int]:
    names = [l["name"] for l in gh_labels]
    linear = ["github-sync"]
    if "bug" in names:
        linear.append("Bug")
    if "enhancement" in names or "feature" in names:
        linear.append("Feature")
    if "epic" in names:
        linear.append("epic")
    pri = 0
    if "priority:high" in names:
        pri = 2
    elif "priority:medium" in names:
        pri = 3
    elif "priority:low" in names:
        pri = 4
    elif "bug" in names:
        pri = 2
    return linear, pri


def build_description(issue: dict) -> str:
    body = (issue.get("body") or "").strip()
    if len(body) > 1500:
        body = body[:1500] + "\n\n… _(truncated — see GitHub)_"
    return (
        f"**GitHub issue:** [#{issue['number']}]({issue['url']})\n\n---\n\n"
        f"{body or '_No description on GitHub._'}"
    )


def create_issue(
    team_id: str,
    project_id: str,
    issue: dict,
    label_names: list[str],
    label_index: dict[str, str],
    priority: int,
) -> str:
    label_ids = [label_index[n] for n in label_names if n in label_index]
    missing = [n for n in label_names if n not in label_index]
    if missing:
        print(f"  warn GH-{issue['number']}: unknown labels {missing}", file=sys.stderr)

    inp: dict = {
        "teamId": team_id,
        "projectId": project_id,
        "title": f"GH-{issue['number']}: {issue['title']}",
        "description": build_description(issue),
    }
    if label_ids:
        inp["labelIds"] = label_ids
    if priority:
        inp["priority"] = priority

    data = linear_graphql(
        """
        mutation($input: IssueCreateInput!) {
          issueCreate(input: $input) { success issue { identifier url } }
        }
        """,
        {"input": inp},
    )
    created = data["issueCreate"]["issue"]
    return f"{created['identifier']} {created['url']}"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    issues = gh_open_issues()
    team_id = find_team_id()
    project_id = find_project_id()
    label_index = label_ids_by_name(team_id)
    have = existing_gh_numbers(team_id)

    to_create = [i for i in issues if i["number"] not in have]
    print(
        f"open on GitHub: {len(issues)} | "
        f"already in Linear (GH-*): {len(have)} | "
        f"to import: {len(to_create)}"
    )

    for issue in to_create:
        label_names, pri = map_labels(issue["labels"])
        title = f"GH-{issue['number']}: {issue['title']}"
        if args.dry_run:
            print(f"  [dry-run] {title} labels={label_names} priority={pri or '-'}")
            continue
        try:
            ref = create_issue(team_id, project_id, issue, label_names, label_index, pri)
            print(f"  ✓ {ref}")
        except urllib.error.HTTPError as e:
            print(f"  ✗ GH-{issue['number']}: HTTP {e.code}", file=sys.stderr)
        except RuntimeError as e:
            print(f"  ✗ GH-{issue['number']}: {e}", file=sys.stderr)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""One-way sync: GitHub Issues → Linear (crabcc project, team VIB).

GitHub is the source of truth. Linear changes do not flow back to GitHub.
Use this script locally or via `.github/workflows/linear-sync.yml`.

Requires:
  LINEAR_API_KEY — Linear → Settings → API → Personal API keys
  One of GH_PERSONAL_TOKEN, GITHUB_TOKEN, GH_TOKEN — or `gh` CLI auth

Usage:
  python3 tools/linear/sync_github_issues.py
  python3 tools/linear/sync_github_issues.py --dry-run
  python3 tools/linear/sync_github_issues.py --issue-number 551
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

REPO = "peterlodri-sec/crabcc"
TEAM_KEY = "VIB"
PROJECT_NAME = "crabcc"
LINEAR_API = "https://api.linear.app/graphql"
GITHUB_API = "https://api.github.com"
GH_TITLE_RE = re.compile(r"^GH-(\d+):")


def gh_token() -> str | None:
    for key in ("GH_PERSONAL_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"):
        val = os.environ.get(key)
        if val:
            return val
    return None


def github_request(path: str, token: str) -> object:
    url = f"{GITHUB_API}{path}"
    req = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read())


def gh_issues_via_api(state: str, token: str) -> list[dict]:
    issues: list[dict] = []
    page = 1
    while page <= 5:
        q = urllib.parse.urlencode(
            {"state": state, "per_page": "100", "page": str(page)},
        )
        batch = github_request(f"/repos/{REPO}/issues?{q}", token)
        if not isinstance(batch, list) or not batch:
            break
        for item in batch:
            if item.get("pull_request"):
                continue
            issues.append(
                {
                    "number": item["number"],
                    "title": item["title"],
                    "state": item["state"],
                    "labels": [{"name": lb["name"]} for lb in item.get("labels", [])],
                    "url": item["html_url"],
                    "body": item.get("body") or "",
                }
            )
        if len(batch) < 100:
            break
        page += 1
    return issues


def gh_issues_via_cli(state: str) -> list[dict]:
    out = subprocess.check_output(
        [
            "gh",
            "issue",
            "list",
            "--repo",
            REPO,
            "--state",
            state,
            "--limit",
            "100",
            "--json",
            "number,title,state,labels,url,body",
        ],
        text=True,
    )
    return json.loads(out)


def fetch_github_issues(state: str = "open") -> list[dict]:
    token = gh_token()
    if token:
        return gh_issues_via_api(state, token)
    try:
        return gh_issues_via_cli(state)
    except (FileNotFoundError, subprocess.CalledProcessError) as e:
        sys.exit(
            "GitHub auth required: set GH_PERSONAL_TOKEN (or GITHUB_TOKEN / GH_TOKEN), "
            f"or install `gh` CLI. ({e})"
        )


def fetch_github_issue(number: int) -> dict | None:
    token = gh_token()
    if token:
        try:
            item = github_request(f"/repos/{REPO}/issues/{number}", token)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None
            raise
        if item.get("pull_request"):
            return None
        return {
            "number": item["number"],
            "title": item["title"],
            "state": item["state"],
            "labels": [{"name": lb["name"]} for lb in item.get("labels", [])],
            "url": item["html_url"],
            "body": item.get("body") or "",
        }
    out = subprocess.check_output(
        [
            "gh",
            "issue",
            "view",
            str(number),
            "--repo",
            REPO,
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
        raise SystemExit(f"project {PROJECT_NAME!r} not found")
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


def workflow_states(team_id: str) -> dict[str, str]:
    """Map 'open' / 'closed' → Linear state id (first match by type)."""
    data = linear_graphql(
        """
        query($teamId: ID!) {
          team(id: $teamId) {
            states { nodes { id name type } }
          }
        }
        """,
        {"teamId": team_id},
    )
    nodes = data["team"]["states"]["nodes"]
    open_id = next((s["id"] for s in nodes if s["type"] in ("unstarted", "started", "backlog")), None)
    closed_id = next((s["id"] for s in nodes if s["type"] == "completed"), None)
    if not closed_id:
        closed_id = next((s["id"] for s in nodes if s["name"].lower() in ("done", "completed")), None)
    if not open_id:
        open_id = nodes[0]["id"] if nodes else None
    return {"open": open_id or "", "closed": closed_id or ""}


def linear_issues_index(team_id: str) -> dict[int, dict]:
    """GH number → {id, identifier, stateType, title}."""
    data = linear_graphql(
        """
        query($teamId: ID!) {
          team(id: $teamId) {
            issues(filter: { title: { contains: "GH-" } }, first: 250) {
              nodes {
                id
                identifier
                title
                state { type name }
              }
            }
          }
        }
        """,
        {"teamId": team_id},
    )
    index: dict[int, dict] = {}
    for node in data["team"]["issues"]["nodes"]:
        m = GH_TITLE_RE.match(node["title"])
        if not m:
            continue
        index[int(m.group(1))] = {
            "id": node["id"],
            "identifier": node["identifier"],
            "stateType": node["state"]["type"],
            "title": node["title"],
        }
    return index


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


def gh_title(issue: dict) -> str:
    return f"GH-{issue['number']}: {issue['title']}"


def create_issue(
    team_id: str,
    project_id: str,
    issue: dict,
    label_names: list[str],
    label_index: dict[str, str],
    priority: int,
    states: dict[str, str],
) -> str:
    label_ids = [label_index[n] for n in label_names if n in label_index]
    inp: dict = {
        "teamId": team_id,
        "projectId": project_id,
        "title": gh_title(issue),
        "description": build_description(issue),
    }
    if label_ids:
        inp["labelIds"] = label_ids
    if priority:
        inp["priority"] = priority
    state_key = "closed" if issue["state"] == "closed" else "open"
    if states.get(state_key):
        inp["stateId"] = states[state_key]

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


def update_issue(
    linear_id: str,
    issue: dict,
    label_names: list[str],
    label_index: dict[str, str],
    priority: int,
    states: dict[str, str],
    existing: dict,
) -> None:
    """Push GitHub fields into Linear (GitHub wins)."""
    title = gh_title(issue)
    state_key = "closed" if issue["state"] == "closed" else "open"
    target_state = states.get(state_key)
    label_ids = [label_index[n] for n in label_names if n in label_index]

    want_closed = issue["state"] == "closed"
    is_closed = existing["stateType"] == "completed"

    inp: dict = {"title": title, "description": build_description(issue)}
    if label_ids:
        inp["labelIds"] = label_ids
    if priority:
        inp["priority"] = priority
    if target_state and want_closed != is_closed:
        inp["stateId"] = target_state

    linear_graphql(
        """
        mutation($id: String!, $input: IssueUpdateInput!) {
          issueUpdate(id: $id, input: $input) { success }
        }
        """,
        {"id": linear_id, "input": inp},
    )


def sync_issue(
    issue: dict,
    *,
    team_id: str,
    project_id: str,
    label_index: dict[str, str],
    states: dict[str, str],
    index: dict[int, dict],
    dry_run: bool,
) -> str | None:
    num = issue["number"]
    label_names, pri = map_labels(issue["labels"])
    title = gh_title(issue)

    if num not in index:
        if dry_run:
            print(f"  [dry-run] create {title} ({issue['state']})")
            return "create"
        ref = create_issue(team_id, project_id, issue, label_names, label_index, pri, states)
        print(f"  ✓ created {ref}")
        return "create"

    existing = index[num]
    if dry_run:
        print(f"  [dry-run] update {existing['identifier']} ← GH-{num} ({issue['state']})")
        return "update"

    update_issue(existing["id"], issue, label_names, label_index, pri, states, existing)
    print(f"  ✓ updated {existing['identifier']} ← GH-{num}")
    return "update"


def main() -> None:
    ap = argparse.ArgumentParser(description="One-way GitHub Issues → Linear sync")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument(
        "--issue-number",
        type=int,
        help="Sync a single GitHub issue (e.g. from workflow issue event)",
    )
    ap.add_argument(
        "--include-recently-closed",
        action="store_true",
        help="Also sync issues closed on GitHub in the last page (state=closed)",
    )
    args = ap.parse_args()

    team_id = find_team_id()
    project_id = find_project_id()
    label_index = label_ids_by_name(team_id)
    states = workflow_states(team_id)
    index = linear_issues_index(team_id)

    if args.issue_number:
        issue = fetch_github_issue(args.issue_number)
        if not issue:
            sys.exit(f"GitHub issue #{args.issue_number} not found")
        issues = [issue]
    else:
        issues = fetch_github_issues("open")
        if args.include_recently_closed:
            issues.extend(fetch_github_issues("closed"))

    created = updated = 0
    for issue in issues:
        try:
            result = sync_issue(
                issue,
                team_id=team_id,
                project_id=project_id,
                label_index=label_index,
                states=states,
                index=index,
                dry_run=args.dry_run,
            )
            if result == "create":
                created += 1
                if not args.dry_run:
                    index[issue["number"]] = {"id": "new", "identifier": "?", "stateType": "", "title": ""}
            elif result == "update":
                updated += 1
        except (urllib.error.HTTPError, RuntimeError) as e:
            print(f"  ✗ GH-{issue['number']}: {e}", file=sys.stderr)

    print(
        f"synced {len(issues)} from GitHub | "
        f"created {created} | updated {updated} | "
        f"linear GH-* indexed {len(index)}"
    )


if __name__ == "__main__":
    main()

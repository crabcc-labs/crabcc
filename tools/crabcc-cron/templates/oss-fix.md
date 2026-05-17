You are working on issue #{N} in {repo}: "{title}".

Body:

Repo root: . (you are already inside the working clone)
Branch:    claude-cron/fix-{N}

Task:
1. Read the issue. If unclear or actually a design discussion → STOP and
   write the literal string "STATUS=no-fix" on its own line followed by
   a one-paragraph reason.
2. Find the failing code/test, OR write a reproducing test if none
   exists.
3. Implement the minimal fix.
4. Run the test command for this repo: {test_cmd}. All must pass.
5. If green → commit. Don't push, don't open a PR (the wrapper does
   that). Final line of your output MUST be "STATUS=fixed".
6. If you can't make tests pass within budget → write "STATUS=tests-failed"
   followed by the diff you tried.
7. If you hit the timeout, the wrapper will mark "STATUS=timeout"
   automatically.

Hard rules:
- Single-file change preferred. Refuse multi-crate refactors.
- No new dependencies.
- Match existing code style; run any formatter the repo configures.
- No telemetry, debug prints, or commented-out code in the final diff.

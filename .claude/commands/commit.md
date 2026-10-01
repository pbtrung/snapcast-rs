---
description: Commit staged/modified changes with a detailed message and push, no AI co-author attribution
---

# Commit and Push

## Steps

1. Run `git status` and `git diff` (and `git diff --staged` if anything is already staged) to see all changes.
2. Format, lint, and test before staging anything (same checks as `make check`):
   - Any `*.rs` or `Cargo.toml` changed: `cargo fmt --all`, then
     `cargo clippy --workspace --all-targets -- -D warnings`, then
     `cargo test --workspace`.
   - If a Cargo feature was added, removed, or renamed, also run
     `cargo check --workspace --all-features` so feature-gated code still builds.
   - Docs-only changes (`*.md`, `.claude/**`) need no checks.
   - If formatting rewrote a file, or any check reports an error, fix it and
     re-run before continuing.
   - Never run tools against `target/` or commit anything under it.
3. If nothing is staged, stage all relevant modified/new files with `git add`
   (include `Cargo.lock` when dependencies changed).
4. Write a **detailed** commit message:
   - Subject line: Conventional Commits style used in this repo
     (`feat:`, `fix:`, `refactor:`, `docs:`, `ci:`, `chore:`), imperative mood.
   - Body: explain _what_ changed and _why_, as bullet points if there are multiple distinct changes.
   - Base the message only on the actual diff — do not include conversational back-and-forth, dead ends, or trial-and-error from the session.
5. Create the commit using a HEREDOC so formatting is preserved, e.g.:
   ```bash
   git commit -m "$(cat <<'EOF'
   refactor: short summary of the change

   - Detail one
   - Detail two
   - Why this change was made
   EOF
   )"
   ```
6. **Do not** add any AI attribution — no `🤖 Generated with Claude Code` line, no `Co-Authored-By: Claude` trailer, no mention of Claude/AI anywhere in the message.
7. Push the commit to the current branch's remote (`git push`, or `git push -u origin <branch>` if it has no upstream yet).
8. Confirm success by showing `git log -1` and `git status` after pushing.

## Rules

- Never include Claude/AI co-authorship or attribution in the commit message.
- Always push after committing — don't stop at just the local commit.
- If the push fails (e.g. diverged branch), report the error and ask before force-pushing or rebasing.
- Never skip clippy warnings with `#[allow(...)]` just to get a commit through — fix the cause.

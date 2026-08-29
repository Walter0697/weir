# Resilient Sync Push Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Weir recover from a concurrent update to its force-pushed sync branch instead of failing before pull-request reconciliation.

**Architecture:** Keep the existing force-push semantics and add retry behavior inside the Git wrapper, where the complete server error is available. A retry is limited to Git’s ref-lock/concurrent-update error; missing objects, authentication errors, and other failures remain fatal. The retry re-invokes `git push`, causing Git to advertise and use the branch’s newest remote tip.

**Tech Stack:** Rust, anyhow, std::process, real temporary Git repositories, Cargo test.

## Global Constraints

- Preserve `git push --quiet --force origin HEAD:refs/heads/<sync-branch>` semantics.
- Do not retry missing-object or unrelated Git failures.
- Do not change pull-request creation ordering: it remains after a successful push.
- Keep tests self-contained with temporary local Git repositories.

---

### Task 1: Retry a concurrent sync-branch ref update

**Files:**
- Modify: `src/git.rs:460-480`
- Test: `src/git.rs` test module near the existing Git command tests

**Interfaces:**
- Consumes: `Git::try_run`, `Output`, and the existing `Cancel` behavior.
- Produces: `Git::force_push(remote, branch)` that retries only when Git reports that the remote ref changed before locking.

- [ ] **Step 1: Write the failing regression test**

Add a test using a bare repository as `origin`, a working repository with a local `HEAD`, and a second clone that advances the remote sync branch after the first push attempt has received the remote advertisement. The test must assert that `force_push` eventually succeeds and that the bare repository’s sync branch equals the working repository’s `HEAD`.

- [ ] **Step 2: Run the focused test to verify it fails**

Run: `cargo test git::tests::force_push_recovers_from_concurrent_remote_update -- --exact --nocapture`

Expected: FAIL because the current one-shot push returns the remote `incorrect old value provided` error.

- [ ] **Step 3: Implement the minimal retry**

Run the same push through `try_run`, inspect the combined Git output, and retry a bounded number of times only when it contains the ref-lock/concurrent-update markers (`cannot lock ref` and `incorrect old value provided`). Reuse the existing cancellation checks between attempts. Return the original Git failure for all other errors.

- [ ] **Step 4: Run the focused test to verify it passes**

Run: `cargo test git::tests::force_push_recovers_from_concurrent_remote_update -- --exact --nocapture`

Expected: PASS, with the remote sync branch pointing to the intended `HEAD`.

- [ ] **Step 5: Run the complete Rust verification suite**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --locked`

Expected: all checks pass without warnings.

- [ ] **Step 6: Commit the implementation**

```bash
git add src/git.rs
git commit -m "fix: retry concurrent sync branch pushes"
```


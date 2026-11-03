## Problem

<!-- One or two sentences. Link the issue: "Closes #n". -->

## Fix

<!-- How you fixed it, in a few lines. -->

## Budgets

<!-- Each budget this touches, with numbers before and after from `make budget` on the reference host. "None" if docs only. -->

## Hit every path

<!-- Say which applied: full build, incremental rebuild, push, manifest and offline, search, exclusion, phone. -->

## New crates

<!-- Each new crate with its binary-size and compile-time cost, or "None". -->

## Checklist

- [ ] `make check` is green locally.
- [ ] The leak check passes. The diff, commits and this body carry synthetic content only.
- [ ] A fresh subagent reviewed the whole diff. Real findings are fixed. False positives are dismissed below with a reason.
- [ ] CI is green and no review thread is unresolved.

## Review

<!-- The reviewer's model and a short summary of its findings, and what you did about each. -->

---

<!-- The model and harness that did the work. -->

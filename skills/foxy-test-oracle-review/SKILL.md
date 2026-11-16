---
name: foxy-test-oracle-review
description: "Review Foxy test expectations, fixtures and acceptance claims for independent evidence, meaningful failures and accidental weakening. Use for material test-oracle changes or confidence audits, not every ordinary unit test."
---

# Foxy test oracle review

An oracle is the evidence that determines the expected result. Read [testing conventions](../../conventions/TESTING_CONVENTIONS.md), the scoped diff, relevant callers and the contract before accepting an implementation or changing its tests. Keep this review read-only unless the brief assigns isolated test work; the main agent owns builds and integration.

## Trace expectation and observation

- Identify the product behavior and where the expected value comes from: a settled contract, known-good independent implementation, fixed fixture or externally observed result. Shared expected/actual helpers and UI/CLI parity through the same faulty core can agree while both are wrong.
- Trace the real observation: exact test selection/count, child exit/result, persisted bytes or native behavior. A logged intention, process existence, skipped/ignored case or timeout is not proof of the intended effect.
- Check identity and freshness across operations: nonce/run, generation, captured game space/instance/owner and fresh output. Restart/rollback/cancellation cases must distinguish stale prior success from the current outcome. A cancellation request alone does not prove that an earlier commit was undone.
- Ask what specific broken behavior would fail this assertion. Prefer one distinguishing negative case over duplicate happy paths. Use an executed before-fix failure or isolated reversible negative control where practical; label source-only reasoning as such. Do not reset a dirty checkout or mutate live user data for this proof.

## Review expectation changes separately

Inspect removed assertions, new ignores/early-success paths, snapshot updates, widened compatibility/baseline bounds, longer timeouts and changed expected generations. Distinguish intentional product change, independently demonstrated fixture correction and implementation regression. Preserve the original failure and justify a corrected fixture against evidence outside the new implementation. Check that the revised case still rejects the relevant wrong result.

Do not weaken an acceptance boundary just to get green results. A legitimate fixture repair needs a concise evidence-based rationale. Use focused independent review where available when changed expectations affect accepted versions, recovery outcomes or unknown-data preservation. Follow existing user authorization for actual scope changes rather than adding a blanket approval step for test maintenance.

## Report the confidence boundary

Return concrete findings with file:line, triggering input, violated contract and consequence. Separate demonstrated defects, untested hypotheses and unavailable evidence. State exactly what the test proves and what remains outside it, such as forced namespace persistence versus graceful saved-game recovery, parser fixtures versus real provider interoperability, or safe compilation versus a correct state machine. No finding is a valid result when the evidence supports it. Stop after the scoped review; do not add a new test suite or broaden the audit without a concrete gap.

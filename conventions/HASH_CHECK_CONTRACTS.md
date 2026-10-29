# Accepted hash-check contracts

These H1-H18 product decisions are settled requirements from the hash-check
review. A future gap review must use them as acceptance criteria and investigate
uncovered transitions or different tradeoffs instead of asking these questions
again. The implementation and testkit evidence do not imply exhaustive coverage.
The layered algorithms remain in [sync conventions](SYNC_ALGO_CONVENTION.md).

| ID | Accepted behavior |
| --- | --- |
| H1 | Quick local checks remain lightweight drift detection. Say that no local changes were detected rather than claiming a fresh full integrity read. Show the check basis and last full-verification time. |
| H2 | Quick local check never contacts the server. Incomplete remote metadata produces an incomplete result with a Refresh action; preserve prior pending state. |
| H3 | Manual Refresh checks fresh remote metadata and performs a quick local check even when the published version matches stored local checksums. Automatic startup still honors its configured options. |
| H4 | Before reusing an update plan, revalidate ordinary local drift across all selected addons, including same-size changed files outside the queue. Verify changed files and widen the selected pending scope as necessary. |
| H5 | Finish accessible files after isolated read failures, but report an incomplete check and distinguish inaccessible files from missing files. Stop promptly when the whole root disappears. Failure alone does not authorize replacement. |
| H6 | Retry a file that changes while being read once. If it keeps changing, leave it unresolved, retain stable results and preserve external bytes. Never persist a mixed read as a stable baseline. |
| H7 | Stage a coherent remote metadata snapshot before authorizing cleanup or transfer. Retry a detected publication race once with fresh metadata, then fail incomplete. Validate membership and names as well as content checksums. |
| H8 | Integrity recheck prefers fresh server metadata, then automatically falls back to available accepted local metadata when the server cannot be reached. Clearly identify the historical/offline basis and incomplete scope; never infer current server freshness. |
| H9 | Cancellation retains complete stable per-file verification evidence for a later ordinary check. Revalidate identity and manifest before reuse. Partial parent rollups never make a cancelled operation clean; a new explicit integrity check still reads all selected files. |
| H10 | Addon folders are managed to the manifest. List unexpected files in the plan and remove them only after Update is approved, with rollback protection. Checking and closing a preview never delete them. Account for sibling manifests before classifying shared files as unexpected. |
| H11 | Managed-file mismatches are ordinary repair work. Offer backup of known local differences before replacement; retain the existing optional automatic backup behavior. Do not claim a checksum proves user intent. |
| H12 | Explicit empty repositories and addons are valid publications. Distinguish them from missing lists, invalid responses and fetch failures. Preserve existing files and expose removal separately; adopting an empty graph never silently uninstalls content. |
| H13 | After full-file verification fails and rollback succeeds, allow one fresh automatic attempt. Refresh metadata and bound this independently of transport retries. A second verification failure is terminal and pending work remains. |
| H14 | User-created links may be checked and repaired only with explicit enrollment of the resolved write target. Show that target and known affected installations before write approval, revalidate it at dispatch and retain operation ownership. |
| H15 | An explicit stronger check can offer to cancel weaker check-only work and start after its workers settle. Never silently interrupt a download or recovery. Deduplicate the replacement request and keep its exact instance identity. |
| H16 | Distinguish a temporary in-memory byte verdict from successfully saved verification. Offer Retry save or an actionable recovery path. A failed save cannot clear durable pending state or become durable clean status. |
| H17 | A manual stalled-read notice offers Keep waiting and Skip this file, with Cancel still available. Skips leave the check incomplete. Unattended checks use a generous bounded no-progress timeout. Do not abandon a worker or let late results overwrite a later operation. |
| H18 | Periodic remote checks are configurable and enabled by default at a 30-minute interval. Run only while idle, never start transfer, and preserve per-instance/space ownership. Always expose the last successful server-check time as a compact timestamp below the repository header. Keep local-check history and verification basis in its tooltip, accessible by hover and keyboard focus. Local checks and failed remote probes must not advance this visible timestamp. |

## Result and recovery requirements

Check type, local completeness, remote freshness and successful persistence are
separate facts. Only complete evidence can clear a pending update. An empty
difference returned because a check was skipped is not a clean result. Preserve
the last conclusive result as historical when a new operation is incomplete.
CLI JSON and exit status must communicate failure/incompleteness even when a
partial byte verdict is useful.

Retained unsaved evidence must be saved or explicitly discarded before another
check or download replaces it. Retry save excludes competing transfers and stops
at the first failed child write, before persisting any dependent parent verdict.
Retry save and Discard are mutually exclusive until the save worker settles.

NotFound proves absence; access denied, I/O failure, a disappearing drive and a
stalled read do not. Do not convert those failures into a confident download
estimate. Retry unresolved scope after access returns. The fingerprint and tree
checksum must describe the same stable bytes. Stable files may be checkpointed
without accepting an incomplete repository rollup.

An offline integrity result identifies its cached metadata basis. Accepted
partial metadata can diagnose only the files it describes; unrepresented files
remain unresolved. Offline evidence cannot authorize a transfer without a fresh
remote gate. A later successful remote refresh reconciles pending state.

Prepare and preview paths are read-only for mod payloads. Approved cleanup must
be recoverable alongside replacements. Never remove a file another known
sibling manifest needs. An empty publication retains previous ownership
information until a separate explicit removal action is approved.

Shared physical targets require explicit write approval and revalidation of
their resolved identity. One game's evidence cannot silently authorize writes
to another game's active files. Same-URL installations at different folders
remain independent; propagation never clears unrelated sibling pending work.
Physical addon write claims use the persistent reserved `.foxy-write.lock` file.
Exclude it from content and cleanup candidates; deleting it can split ownership
between processes holding different file handles.

## Validation scope

For temporal claims use isolated synthetic origins and configuration roots.
Assert intermediate status, files, durable evidence, pending work and the next
retry/restart. Run a second unchanged check to detect recurring recovery work.
An oracle after a repaired download proves the payload, not the earlier check's
terminal status. Fault-inject publication changes, read/save errors, cancellation
and stalled reads with deterministic barriers where practical.

UI changes require keyboard traversal, visible focus and activation checks;
status meaning must not depend on color. New CLI behavior preserves stable JSON,
exit-code and destructive-action confirmation contracts.

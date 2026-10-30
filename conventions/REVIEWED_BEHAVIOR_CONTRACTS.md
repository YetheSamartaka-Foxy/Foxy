# Additional accepted behavior contracts

R01-R24 are settled product answers. Enforce them alongside N01-N18 in
[lifecycle contracts](BEHAVIOR_CONTRACTS.md), Q1-Q18 in
[core conventions](CORE_CONVENTIONS.md), and H1-H18 in
[hash-check contracts](HASH_CHECK_CONTRACTS.md). Future gap reviews must target
different transitions or materially different tradeoffs. Do not repeat these
questions or substitute current implementation/tests for these requirements.

| ID | Accepted behavior |
| --- | --- |
| R01 | Unresolved recovery quarantines affected physical targets and uncertain overlaps. Proven independent targets remain usable. Retain journals and pending evidence until recovery settles. |
| R02 | Ordinary settings saves retain independently successful application or game-space writes. Explain which half saved and which remains unsaved. Retry/discard affects unsaved changes; configuration imports retain N05's atomic contract. |
| R03 | One operational Foxy process owns a configuration root, even when another process targets a different game space. Claim before loading, migrating or saving shared state. Required helper/read-only integration commands remain explicit exemptions. |
| R04 | Preserve divergent readable legacy and game-space sources. Open the existing space with a persistent review notice and explicit recovery/comparison route. Never automatically merge, overwrite or move payloads. |
| R05 | Same-URL/same-folder clones are separate launch presets over shared physical evidence. Readiness describes each preset's enabled addons; a lean check cannot establish expanded-preset readiness. Deduplicate only compatible physical work. |
| R06 | Keep the outgoing workspace visibly active until the target can load coherently. Failed target preparation preserves the outgoing setup, explains failure and permits retry/cancel. Never report incomplete recovery as a successful activation or leak old results. |
| R07 | Close may discard a deferred selection check after saving the selection, which remains visibly unchecked. Honor disabled startup checks. Historical evidence for another selection cannot establish readiness. |
| R08 | Proxy changes during accepted work offer Apply to new work or Restart affected work now. Restart waits for cancellation/recovery and preserves exact instance and approved scope. Never expose credentials. |
| R09 | Scheduled close/shutdown waits for the finite set of jobs already due when its post-action is armed. Preserve each job's outcome/success conditions and resolve conflicting actions explicitly. Later recurrences cannot postpone the captured batch indefinitely. |
| R10 | Recheck gameplay immediately before OS shutdown handoff, after persistence/recovery. New detected activity requires renewed confirmation; unavailable detection is explicit. Never terminate a game automatically or issue real shutdown in tests. |
| R11 | A queued launch for another instance offers Replace or Keep. Retain one confirmed exact-instance/server continuation and check readiness freshly for the final request. |
| R12 | Failed final Join queries retain the reviewed request with Retry and Cancel. Preserve compatible choices but block dispatch until fresh requirements succeed. |
| R13 | Progress describes current phase and attempt. Label resets for fallback/repeated work, track total transfer and attempt context separately, and distinguish transfer from verification, durable save and settled recovery. |
| R14 | Older builds preserve valid unknown settings values in their original application/game files when saving known settings. Do not silently discard or relocate opaque fields. Uninterpretable formats require recovery rather than guessing. |
| R15 | A detected device change behind an unchanged ordinary target path requires review before writing. Read-only inspection is allowed. Preserve pending/pause/evidence until explicit attachment or path choice and revalidate at dispatch. |
| R16 | Before accepting Restore during a pending automatic retry, offer Restore and stop retrying or Keep retrying. Accepted restoration revokes the automatic retry, retains pending-update visibility and serializes writers. |
| R17 | A remotely removed space member stays local until reviewed. Offer Keep in this group or Keep as standalone without moving/deleting files. Show unresolved membership in later bulk previews; accepted runs retain frozen-target rules. |
| R18 | Do not add a second operational process or cross-process wait queue. R03 excludes that workflow. Retain physical addon claims for independent roots/external interference; never bypass or delete their lock files. |
| R19 | Full integrity reads remain manual. Show historical timestamps and check basis. Age alone neither starts a full read, blocks launch nor declares corruption. |
| R20 | A nonexistent recurring local time runs once at the first valid instant after the gap; a repeated time uses its first occurrence once. Display resolved next-run time, persist occurrence identity, and preserve cancellation/post-action gates. |
| R21 | An extra-file destination changed after successful activation offers Reapply template or Keep this file for this launch. Keep the durable template unless explicitly replaced. A failed attempt restores its own pre-attempt bytes. |
| R22 | Extra files support reviewed ordered layers, later entries winning. Preview conflicts and review enabled/imported/reordered sources that change the winner. Preserve ordering across pack round trips; identical content may deduplicate. |
| R23 | Refresh all Workshop pins retains the previous active selection unless every requested snapshot and the final store save succeed. Stage new snapshots, preserve old payloads, and recover coherently after failure/restart. Do not imply cross-mod release compatibility. |
| R24 | Preserve Total War pack-name deduplication and launch behavior. No change to that area was authorized by this review; do not repeat its collision-policy question. |

## Evidence requirements

Use synthetic origins and isolated configuration roots. For temporal claims
assert intermediate files, configuration, database/pending state, operation
ownership, visible outcomes, and the next retry/restart. A passing helper or
post-repair payload oracle does not establish earlier consent or progress.

New UI decisions require keyboard reachability, visible focus, activation and
readable status without color. Keep CLI previews read-only, explicit destructive
consent, stable JSON/exit codes and documented recovery alternatives. Update
examples for persisted formats. Changelog entries describe behavior rather than
translation bookkeeping.

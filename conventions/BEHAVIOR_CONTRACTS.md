# Accepted lifecycle, ownership and launch contracts

N01-N18 are settled product decisions from the behavior review. Future gap
reviews must enforce these contracts, Q1-Q18 in [core conventions](CORE_CONVENTIONS.md)
and H1-H18 in [hash-check contracts](HASH_CHECK_CONTRACTS.md). Investigate new
transitions, integrations and failure paths instead of asking the same product
questions again. Reopen a decision only for a materially different tradeoff or
an explicit user request. Existing code and passing tests are evidence, not an
alternative source of product intent.

| ID | Accepted behavior |
| --- | --- |
| N01 | An active managed-file update owns its targets until recovery settles. Cancellation restores the pre-update version even if an external process edited a promoted target. Restoration failure remains unresolved recovery, never successful cancellation/readiness. |
| N02 | Delete downloaded files only for the selected `(URL, local path)` instance after physical ownership checks. Another same-URL installation in a separate folder is independent. Protect shared, aliased and overlapping targets; preserve other instances' records, bytes and pending state. |
| N03 | Workshop Remove unregisters from the active space and may delete that space's private frozen copies. Steam unsubscription and deletion of shared live content are separate explicit actions. Removing a space-local registration cannot silently affect another installation's Steam content. |
| N04 | Install and Restart queues a visible cancellable request until accepted work, recovery and outgoing persistence finish. Retain the verified installer if preparation or handoff fails. Never exit directly from a busy installer button. |
| N05 | A config-pack import applies completely or restores the previous coherent setup. Include repository/space JSON, Workshop/Reforger/extra-file stores and replaced payloads. Retain exact-space recovery intent through final config saves; recover interrupted imports before loading their mixed state. |
| N06 | Failed extra-file launch preparation offers Retry, explicit Continue for this launch, or Cancel. Default CLI execution fails unless incomplete preparation was explicitly allowed. Preparation failure is distinct from launch handoff success. |
| N07 | A missing frozen Workshop snapshot requires explicit one-launch consent before using live files. Preserve the durable pin, warn again next launch while unavailable, and report the actual version/path basis. Unrelated or disabled pins do not block a launch. |
| N08 | A successful command handoff is the launch completion, history and close/hide boundary. Say the launch request was sent; do not claim the game is running or a server connection succeeded. Failure before handoff cannot update successful launch history. |
| N09 | A definite pre-handoff failure or cancellation restores launch preparation files before reporting completion. Include generated manifests and managed extra files. A failed restore retains recoverable evidence. Do not roll back files after the game has accepted ownership through a successful handoff. |
| N10 | Direct Download previews collisions and requires replacement approval. Stage replacements so failed or cancelled transfers retain the old complete files. Leave unrelated files untouched and never create managed clean evidence. Refuse overlap with managed installations rather than bypass their ownership and verification rules. |
| N11 | Prefer backups with matching game-space/repository-instance origin. Cross-origin and unknown legacy restores require source/destination review and explicit consent. Preserve old backups and associate deduplicated snapshots with all known origins. Apply the same origin model to retention groups. |
| N12 | Cleanup previews the exact deletion set. Deleting the final recovery copy requires a distinct confirmation. Count and age rules apply after approval; report actual removals and failures, without broadening the reviewed set on a later rescan. |
| N13 | Before removing an inactive space, list its private frozen mods and extra-file payloads and offer preservation/export. Failed preservation prevents destructive removal. External game/repository downloads and other spaces remain untouched. CLI removal requires an explicit private-payload preservation/discard choice when such data exists. |
| N14 | Importing a Workshop bundle into a populated space previews selection differences and offers Merge or Match. Merge applies incoming enabled settings and load order and retains unlisted selections. Match adopts the incoming enabled selection/order and disables unlisted items without deleting their payloads. |
| N15 | Clipboard profile import adds a profile without selecting it. Preserve the current active profile; selecting the imported profile later follows the existing deferred-check/readiness rules. |
| N16 | When an addon moves between required and optional categories, adopt the new published category default for Default and saved profiles and clearly report effective selection changes. Preserve overrides within an unchanged category. Do not enlarge an already-approved transfer scope. |
| N17 | A different explicit Launch/Join request for the same queued instance asks whether to replace the saved action. Show the old/new choices and retain exactly one confirmed continuation. Fresh readiness applies to the final request; stale results cannot resurrect a replaced action. |
| N18 | New detected gameplay during a scheduled shutdown countdown requires renewed confirmation. Preserve unattended behavior when no new activity occurs. Detection is capability-based; unavailable detection must be stated, not treated as evidence no game is running. Never terminate a game automatically or bypass persistence/recovery. |

## Validation and review

Use isolated synthetic origins and configuration roots. Temporal claims require
observable file/config/database/pending state, the interruption boundary, and a
next retry or restart. Distinguish helper assertions from complete UI/CLI flows.
An oracle verifies final payload bytes, not earlier consent, progress or saved
selection. Report partial/incomplete results and untested branches explicitly.

Config-pack recovery claims the exact space through its final config saves.
Launch recovery restores definite pre-handoff failures; an interrupted dispatch
whose handoff result is unknown retains its prepared files and original snapshots
under `launch_preparation_retained` for review. Bundle payload imports stage all
entries into unique private directories before one atomic Workshop-store save.
Cleanup revalidates its reviewed inventory, and space removal refuses another
process's database ownership claim. Payload preservation and cleanup run outside
the UI thread; installer and shutdown settlement wait for these accepted workers.

User-facing decisions require keyboard reachability, predictable focus, activation
and readable status without color. Preserve stable CLI JSON/exit codes, previews
without writes and explicit destructive `--yes` consent. Add the user guide and
examples when public contracts or persisted formats change. Do not duplicate this
decision table in root agent instructions or private quiz artifacts.

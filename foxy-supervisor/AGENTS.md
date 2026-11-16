# Supervisor crate

Read `../conventions/CORE_CONVENTIONS.md` and `../conventions/TESTING_CONVENTIONS.md` before changing this crate. It contains game-independent ownership primitives, not game launch policy or detached supervision.

- Keep native handles private and non-cloneable. A PID is diagnostic data, never process authority.
- No child execution before containment and token checks succeed. Unavailable containment fails closed.
- Controlled stdio uses pre-opened native handles; prepare Linux descriptors before clone and keep child actions async-signal-safe. Soft-stop delivery uses retained process authority, never a host PID lookup. Stop timeout retains ownership; forced cleanup needs explicit intent or the separately accepted foreground-exit policy.
- Wait for the entire contained tree before orderly lifetime-lease release. Emergency Drop and owner death are forced, save-uncertain outcomes.
- Installation locks use physical directory identity in one stable same-user namespace, independent of config roots. Never mutate adopted installations or unlink lock files.
- Read-only support/preflight must not create a lock namespace or claim locks.
- Linux uses a retained creator thread and an unprivileged user/PID namespace init, with pidfd authority and waited namespace teardown. Root/capability-bearing owners and unavailable namespace APIs fail closed. The reported host PID identifies init, not the payload. Keep all post-clone child work allocation-free and async-signal-safe.
- Persist a strict consumer registration before execution. Retire only the exact owned record after witnessed tree cleanup; failed or uncertain spawn/cleanup retains the recovery barrier. Maintenance requires exclusive content and no registrations. Inspection never proves liveness or grants maintenance; no PID-only reconciliation or automatic marker deletion.
- Native Linux positive proofs are ignored by default because namespace policies vary. Explicitly run `cargo test -p foxy-supervisor native_ -- --ignored --test-threads=1` as an ordinary user on an accepting Linux kernel. Compiler checks alone do not establish containment or game compatibility.
- Subprocess fixtures are Rust tests in temporary directories. Helpers are ignored in ordinary runs and invoked only by their parent tests. The integrating agent runs all builds and checks.

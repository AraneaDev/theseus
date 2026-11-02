# Recovery and owned cleanup

Keep the saved report and journal from the [run](../get-started/first-run.md).
Docker must be reachable for resource cleanup, while [report
viewing](../reference/evidence.md) is offline. A failed expectation, incomplete
observation and cancellation have distinct meanings; do not infer recovery
authority from an exit code alone.

## Interruption and forward recovery

Headless SIGINT/SIGTERM requests cancellation and returns the saved partial
report. A verified planned interruption has outcome `interrupted`, no
application exit status, and independent readiness/stopped proof. Restart
evidence records the same container ID and independently verified running state.
Exit 137 alone, stdout markers, or an ordinary timeout cannot establish
deliberate interruption. Early ordinary completion (even exit 0) fails the
barrier contract and skips recovery. Missing/stale/symlink receipts, control
errors, timeouts and unverified restart are incomplete; recovery/data failures
are failed expectations. Cancellation wins, returns 130 and prevents later
recovery. Running application failures still permit declared uninstall;
unavailable containers cannot provide uninstall evidence.

This models an abrupt whole-container stop at one cooperative pause point. It
does not establish host power-loss durability/fsync behavior, arbitrary
installer interleavings or service-manager behavior. Recovery is an explicit
forward application command; ordinary timeout/cancellation never retries or
recovers automatically. Rollback after an interrupted upgrade uses a separate
schema-6 scenario with explicit rollback and known-good proof.

## Declared downgrade outcomes

Downgrade chooses its branch from the declared policy. A compatible attempt must
return zero; a rejection policy must return its exact declared status with
verified bounded stdout. Unexpected success, wrong status or marker fails. A
missing completion receipt, Docker transport/control failure, truncated output,
timeout or cancellation cannot establish refusal, even if text contains the
marker. An `expected_rejection` command outcome records the real status and
marker proof, but the scenario passes only with all required binary/data/runtime
checks. Live and offline UI distinguishes “Compatible downgrade passed” from
“Declared rejection checks passed” and shows actual status, marker verification,
scopes and comparison stages. Safe rejection only establishes these observed
scopes; it does not prove arbitrary installer safety. Downgrade does not
implement rollback: schema 5 supplies its separate explicit restoration
contract.

## Rollback authorization

Rollback authorizes reversal only after the exact declared application failure
is proved by a normal completion receipt, bounded complete stdout and a matching
literal marker, plus all selected Changed checks. Unexpected exit 0, wrong
status or marker fails. Missing receipt, transport errors or truncated stdout
cannot authorize rollback; timeout/cancellation does not trigger it. An
`expected_failure` command records the real nonzero status and marker proof. A
scenario can pass only after all required binary/runtime/data checks, generated
backup checks where applicable, backup cleanup, uninstall and owned teardown
also pass. A zero-return rollback command alone cannot establish restored data.
Theseus verifies observed bytes/metadata; application authors own quiescence and
application-specific backup and restoration, including database consistency. No
host backup/export, atomic snapshot or power-loss durability is claimed.

## Interrupted rollback

Interrupted rollback follows the same known-good/backup prefix, captures
`before_interrupt` immediately before `interrupt_upgrade`, verifies stop and
same-ID restart, captures Interrupted and its checks, then captures fresh
`before_rollback` before the shared rollback/cleanup tail. Actual Changed
compares `before_interrupt` to `interrupted`; restoration and fresh mapped
backup readiness compare original `before_upgrade`. No observation is claimed
while stopped. Verified stop/restart, complete observations, actual mutation,
every prerequisite and fresh backup readiness are all required. Missing proof is
Incomplete; known predicate mismatch is Failed; cancellation retains precedence.
A prior successful backup check cannot authorize restoration after later
check-induced corruption. The backup root is never deleted if it predated the
run; cleanup is authorized only when absent-root verification passed and backup
actually started. See the
[walkthrough](../../examples/interrupted-rollback/README.md) for the mapped
check list. This declared cooperative barrier does not establish arbitrary fault
timing or host/database durability; authors own quiescence and database
consistency.

## Cancellation and owned cleanup

Timeout/cancellation removes the owned scenario container, including
descendants; it cannot promise application uninstall observations after that
removal. If the daemon is unavailable, separately bounded cleanup may fail and
leave journaled resources. There is no global Docker prune.

Log references and recovery journals are confined and identity-checked as
described in [evidence and
containment](../reference/evidence.md#log-privacy-and-containment). Use the
report path printed by the run; from its recipe project, for example:

```sh
theseus report .theseus-runs/RUN_ID
theseus cleanup .theseus-runs/RUN_ID
```

## Related documentation

- [Recipes](../reference/recipes.md): exact steps, checkpoints and required checks.
- [Evidence](../reference/evidence.md): exit codes, retained paths and privacy.
- [Interrupted rollback example](../../examples/interrupted-rollback/README.md): original and fresh restoration references.
- [MCP shutdown](../agents/mcp-server.md): unresolved server work and explicit saved-resource recovery.

[Documentation index](../README.md) · [Project README](../../README.md)


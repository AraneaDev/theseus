# Services and desktop applications

Service and desktop integration is under development on the native-adapter
branch. See the [roadmap](../../ROADMAP.md) for source implementation, review and
native acceptance status. These checks extend the supplied-target workflow in
[native targets](native-targets.md); they require explicit target authority.

## What the installer must do

The installer owns service registration, configuration and removal, and desktop
entry installation and removal. Theseus observes those results and performs only
the Start, Stop, activation and cooperative-close rehearsals declared by the
recipe. It does not repair a missing service or entry to make a check pass.

Service Clean, Upgrade and Reinstall use the ordinary scenario sequence and
original provider reservation. Owned desktop Clean, Upgrade and Reinstall are
locally exercised through the CLI and fresh MCP sessions. Combined service and
desktop scenarios share one sequence and lease; their acceptance status is
tracked on the roadmap. Upgrade and Reinstall compare the selected executable,
configuration and retained data. A recipe can declare either a Running or a
HarnessStopped runtime precondition before a SUT transition. HarnessStopped is
reported as a rehearsal control; it cannot establish that the installer stopped
the service itself.

A known failed required fact remains a failure even when cleanup succeeds.
Uncertain manager state or unproved process settlement is reported separately
and can keep the overall result incomplete.

## Supply the manager or session

| Integration | Required authority | Qualification boundary |
| --- | --- | --- |
| systemd | Exact manager endpoint, principal, generation and definition roots | Private-model checks do not establish actual systemd acceptance. |
| launchd | Explicit user domain, label, original definition and qualified native tool/output format | No default GUI domain or service search. |
| Windows SCM | Original service registration, explicit helper account, paths and ACL policy | Administrator membership alone does not prove the admitted rights or original registration. |
| Desktop | Supplied broker endpoint, account, session ID and epoch, display endpoints and allowed modes | No implicit broker startup, login, display selection or elevation. |

The provider binds its receipt to the original target ownership, workspace and
manifest. A desktop broker has its own durable ledger and authenticates the
helper connection. A lost reply is recovered by querying the original accepted
identity. It does not authorize another activation.

The desktop manifest commits an `exclusive_quiescent` namespace policy. The
operator must keep entry/executable objects, their ancestor mappings and IPC
endpoints stable during effects. Declared installer generation changes happen
at the scenario boundaries. This policy is a prerequisite supplied by the
operator; it is not a kernel-enforced lock against concurrent writers.

## Read each fact separately

A valid entry, accepted activation, application readiness, primary exit and
settled descendants establish different facts. An activation receipt cannot
supply missing readiness or process-tree evidence.

The authenticated application challenge reports the actual image, version and
configured data, plus an explicit instance and initialization state. These are
application-reported facts. A cooperative-close acknowledgement is separate from
observed process exit and descendant settlement.

OwnedExecutable uses a directly retained process scope. NativeActivation uses
the admitted GIO, AppKit or Windows Shell API and reports only the ownership
that API actually provides. A callback PID is correlation data; it does not
create termination or process-tree authority. If the selected launcher cannot
meet a required lifetime contract, admission must refuse before activation.

The macOS product currently refuses OwnedExecutable before execution: its
process-group cleanup cannot prove the descendant settlement required by the
current broker release contract, even when the recipe asks only for primary
exit. Use the native AppKit manager-instance contract for a qualified supplied
session. Supporting owned-primary-only completion requires a separate explicit
retirement contract; it must not turn unknown descendants into a settled tree.

The native lifetime ceilings are intentionally different:

| Launcher | Admissible native lifetime authority | Does not establish |
| --- | --- | --- |
| Qualified Linux GIO child branch | Original child retained before it can be reaped | Descendant ownership or arbitrary callback-PID adoption |
| macOS AppKit | Original returned application object's termination | Primary process exit or descendant settlement |
| Windows Shell | Original returned process HANDLE, when present | A job, process tree or lifetime when Shell returns no HANDLE |

For native modes, the executable environment commitment bounds the broker's
complete startup environment; it does not claim that a native manager replaces
the application's environment. Linux and macOS use the working-directory commitment
for the broker's startup directory and make no application-directory promise.
Windows additionally checks the shortcut's exact application working directory.
OwnedExecutable retains its direct command environment and directory contract.

These native routing paths remain under integration. A serialized SDK receipt or
artifact digest is a commitment to check, not authenticated qualification by itself.
The Linux provider must qualify the complete declared GIO/GLib dependency graph,
helper and every application tuple during its original build/reset preparation.
That private bootstrap is bound to the same ownership, manifest and broker session;
the controller checks the authenticated admission before acquiring a lease.
Keep the SDK namespace protected and quiescent for the run. Declaring a digest
in a recipe or broker JSON cannot create this authority.

Missing qualification or an unsupported required lifetime must refuse admission.
A late native return retains its original owner and uncertainty; it does not prove
that cancellation completed within its deadline.

Source-only headless fixtures establish owned execution and IPC behavior. They
do not establish native launcher activation, a graphical login session or window
visibility. Production launch/configuration loaders reject their headless
session kind, including when fixture features are compiled together.

## Preserve evidence and recover original owners

Keep the complete run directory and original intent store. Schema2 reports and
journals carry integration facts alongside catalog evidence. Saved inspection
must remain read-only when the target, broker or original executable is no
longer available.

Recovery verifies the original launch and intent, then queries, cancels or
releases original operations where authority remains available. It cannot
reinstall, restart, recreate an entry or adopt a process by PID. Replaced
registrations, changed sessions and lost native owners retain their uncertainty.
Provider disposal and local owner settlement remain separate results.

See [owned cleanup](../operate/recovery.md), [evidence limits](../reference/evidence.md)
and [validation scope](../contribute/validation.md). Actual native manager tests
and supplied interactive-session acceptance remain separate roadmap gates.

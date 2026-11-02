# Installation and controller scope

Run the build commands from the repository root of a source checkout. Theseus
runs Linux guests through Docker; the controller runs on the native host. Start
with this page to check prerequisites, then follow [your first
run](first-run.md).

Theseus 0.12 targets native Linux, macOS and Windows controllers with a current
stable Rust toolchain:

```sh
cargo build --workspace --release --locked
cargo install --path . --locked
theseus --help
```

Docker CLI and a reachable Docker daemon are required for execution and cleanup.
Validation and saved report viewing work offline. Docker access is configured by
you; Theseus does not change daemon settings or user groups. Recipes are
intended for maintainer-controlled inputs. A container is not a hostile-code
sandbox.

## Native controllers and release qualification

Release acceptance requires the same source to pass all three native host jobs
and all eight Linux Docker suites. Native inherited-stdio qualification must
cover both ordinary Rust launchers and CPython `Popen` / `CreatePipe` launchers,
alongside actual-binary suites. Every required launcher must pass; a passing
Rust launcher alone does not establish platform acceptance. Consult the
[source-specific CI
results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml).

Default push and pull-request CI requires the full application workspace on:

| Runner | Native architecture / Rust host | Filesystem qualification |
| --- | --- | --- |
| Ubuntu 24.04 | x86-64 / `x86_64-unknown-linux-gnu` | Local Linux filesystem |
| macOS 15 | arm64 / `aarch64-apple-darwin` | Local APFS; strict roots reject aliased ancestors |
| Windows Server 2025 | x86-64 / `x86_64-pc-windows-msvc` | Local NTFS for writable evidence |

Each native job asserts its compiler host and records the compiler, image and
actual OS version/build. The Windows runner has reported build 26100; the runner
label alone does not establish runtime acceptance. Release acceptance requires
all three native jobs and all eight Ubuntu Docker gates on the same 0.12 source.
Execution on Windows builds before 26100 remains unverified. Other
architectures, network filesystems and native macOS/Windows guest environments
are outside this initial scope.

Unix private creation, no-follow traversal and sync-error propagation remain in
effect. Windows evidence uses retained identity/reparse checks, file flushing
and atomic publication on local NTFS; this is not a claim of power-loss
directory persistence. Strict macOS fixture roots use recorded canonical
temporary ancestry; normal-root alias acceptance and strict-root rejection are
tested separately. Native names are filesystem qualified: Linux tests include
invalid UTF-8 and Windows tests include unpaired UTF-16, while APFS rejects
invalid UTF-8 creation. Stored report and artifact references remain validated
UTF-8 with POSIX `/` paths. Windows input execute permissions are declared guest
data; see [input materialization](../reference/recipes.md#inputs-and-execution).

Native CLI and PTY/ConPTY tests cover actual application input, resize, errors,
terminal mode restoration, EOF and owned cleanup. Windows records exact active
input/output mode words for default and adverse starting profiles before harness
cleanup. Its real pressure/EOF/job/close tests also use a controlled
blocking-close **MODEL** to check drain/join ordering; that model does not
establish older-kernel behavior. The Linux-only first-scene `LD_PRELOAD`
write-error injection is supplemental; native engine-error restoration remains
required on every host. The MCP server has a separate ordinary inherited-pipe
qualification and actual binary suite; the coupled terminal transport does not
establish that acceptance. Release acceptance requires same-source
Linux/macOS/Windows inherited-stdio qualification and actual-binary suites;
consult the source-specific [required CI
results](https://github.com/AraneaDev/theseus/actions/workflows/ci.yml). Native
guest adapters, service/session/GUI or VM behavior and external real-client
prerequisites remain separate acceptance work; Linux Docker tests do not
establish them.

## Related documentation

- [First run](first-run.md): validate a recipe and explicitly start a run.
- [Validation](../contribute/validation.md): run the required native and Docker gates.
- [Recipes](../reference/recipes.md): guest images, input permissions and environment.
- [MCP server](../agents/mcp-server.md): inherited-pipe qualification is a separate contract.

[Documentation index](../README.md) · [Project README](../../README.md)


# Containment for model-controlled processes

> Status: design note. Ferrum does not implement the containment profiles described here, and this document is not a roadmap commitment. The current security contract remains documented in [security.md](security.md) and [tool-authority.md](tool-authority.md).

Ferrum currently reduces tool authority with deterministic policy. It parses shell commands, limits exposed tools, bounds output and execution time, checks recognized mutation paths, and cleans up process trees. Those controls prevent many accidents, but they do not isolate an allowed program from the host.

A permitted `cargo test` can run a build script or test binary. That program can make system calls unrelated to the command text Ferrum inspected. It may read the user's home directory, modify files outside the checkout, use inherited credentials, or connect to the network. No shell parser can infer and contain all of that behavior.

This note explores a separate containment layer for model-controlled tools.

## The intended guarantee

Use a hostile build script as the acceptance test:

> Ferrum may allow `cargo test`, but the kernel must still prevent its descendants from reading host credentials, writing outside approved locations, changing `.git`, opening disallowed network connections, or surviving after the tool call ends.

The shell policy and containment layer would have different jobs:

- **Policy** decides whether Ferrum should attempt an operation and returns useful errors for recognizable hazards.
- **Containment** limits what an allowed operation can actually do, including behavior hidden inside scripts, plugins, compilers, tests, and other executables.

Shell parsing remains useful as an accident-prevention and intent-validation layer. It must not be presented as the isolation boundary.

## Possible profiles

The names below are illustrative rather than proposed configuration syntax.

| Profile | Filesystem | Network | Intended use |
| --- | --- | --- | --- |
| `host` | Invoking user's normal access | Normal host access | Explicitly trusted work; equivalent to today's unisolated execution |
| `inspect` | Checkout read-only; private home and temporary directory | None | Reviewing an untrusted checkout |
| `workspace` | Checkout writable, `.git` read-only, selected toolchains read-only, private build/cache directories | None by default | Building and testing without granting normal host authority |

Network could be a separate setting such as `none`, `loopback`, or `full`. Selecting `full` would be an explicit authority decision, not an automatic consequence of allowing a build command.

Safety and containment should remain visible as independent state:

```text
safety: medium
isolation: workspace
network: none
```

A restrictive shell policy without process isolation and an isolated process with an overly broad writable workspace solve different problems. Ferrum benefits from both.

## Execution architecture

Ferrum's provider process needs access to provider authentication, sessions, configuration, and the provider network endpoint. It should therefore remain outside the tool jail. Model-controlled work should cross a narrow boundary into a separate worker.

A possible design is:

1. The main process validates the tool call and selects a user-configured containment profile.
2. It starts a small tool worker with no inherited credentials or ambient file descriptors.
3. The worker enters its filesystem, environment, process, and network restrictions before handling the request.
4. Native filesystem tools and shell descendants execute inside those restrictions.
5. The worker returns bounded output over a pipe and is destroyed after the operation.

Putting native `read`, `write`, `edit`, and search operations behind the same worker matters. Restricting only Bash would still leave the main process capable of performing a model-requested read outside the workspace. Existing canonical-path and identity checks should remain as defense in depth, but the selected profile should also determine which paths the worker can see.

A per-call worker is simple and limits retained authority. A longer-lived worker may reduce startup cost but creates more state and cleanup obligations. The performance difference should be measured before choosing.

## Filesystem view

An `inspect` or `workspace` worker should not see the user's normal home directory. Its filesystem view would contain only what the operation needs:

- the checkout, read-only or writable according to profile;
- `.git` mounted read-only unless the user explicitly requests Git mutation outside the contained model workflow;
- required system libraries and toolchains mounted read-only;
- a private `HOME` and `TMPDIR`;
- dedicated build and dependency caches without host credentials;
- explicit additional roots selected by the user.

Directly exposing `~/.cargo`, `~/.ssh`, cloud configuration, Vault state, provider authentication, or the Ferrum config directory would defeat much of the boundary. Private dependency fetching may eventually need a separate credential-broker design; copying host credentials into the worker is not a safe default.

A writable checkout can still be damaged. A stricter future mode could build in a copy-on-write view and apply reviewed changes afterward, but that is separate from the first containment milestone.

## Environment and inherited authority

The worker should start from an empty environment and receive a small baseline such as its controlled `PATH`, `HOME`, `TMPDIR`, locale, and terminal settings. Build-specific variables should require explicit configuration.

It must not inherit:

- provider API keys or OAuth material;
- SSH or GPG agent sockets;
- Vault, cloud, or package-publishing credentials;
- Docker or Podman control sockets;
- D-Bus, Wayland, X11, browser-debugging, or desktop-session authority;
- unrelated open file descriptors.

A contained shell should not load the user's login profile. A controlled invocation such as `bash --noprofile --norc -c` is easier to reason about than `bash -lc`, provided Ferrum supplies the required environment explicitly.

## Kernel mechanisms

Ferrum is Linux-only, so Linux-native enforcement is appropriate. Two mechanisms are worth evaluating rather than assuming either one is sufficient.

### Bubblewrap

Bubblewrap can construct a private mount view, bind selected paths read-only or writable, create private home and temporary directories, and isolate network and process namespaces. It is well suited to a read-only `.git` view and a private `/proc`.

Its availability depends on the installed helper and the host's user-namespace policy. Ferrum would need capability detection and clear startup errors.

### Landlock

Landlock can apply unprivileged, inherited filesystem restrictions from inside the worker. Supported access rights depend on the running kernel's Landlock ABI; newer kernels also provide some network controls.

Landlock avoids relying on a separate sandbox helper, but it does not create a private filesystem view and may not express every desired mount exception cleanly. It is a candidate built-in backend or additional defense, not a reason to skip capability probing.

### Supporting controls

Regardless of the filesystem backend, the launcher should also:

- set `no_new_privs` and drop capabilities;
- close unrelated file descriptors;
- use process and mount namespaces where available;
- apply resource limits and the existing output bounds;
- place the complete descendant tree in a cgroup when available;
- kill all remaining descendants when the tool call ends.

Ferrum already reports `cgroup_v2` or `process_group` as command containment. That currently describes descendant tracking and cleanup, not filesystem, credential, syscall, or network isolation. Future status output should distinguish these concepts, for example:

```text
process cleanup: cgroup_v2
execution isolation: none
```

## Network policy

`none` should be the default for isolated execution. It makes builds less convenient, but it produces a meaningful guarantee and prevents a hostile build script from becoming an exfiltration client.

Useful explicit modes may include:

- `none`: no network namespace access;
- `loopback`: local services only;
- `full`: normal network access, clearly marked as a reduced containment posture.

Dependency downloads need deliberate handling. Options include a user-initiated fetch step, a dedicated uncredentialed package cache, or temporarily selecting broader network authority. Domain filtering alone should not be treated as a complete security boundary.

The main Ferrum process still needs provider connectivity. Child network isolation must not be confused with preventing the model provider from receiving files that Ferrum deliberately reads and places in context. The primary protection against that path is keeping sensitive files outside every model-readable root.

## MCP and user-invoked commands

MCP servers have tool-specific needs: a browser controller may require network and desktop access, while a documentation server may not. Enabling a host-authority MCP server would make the effective session only partially contained. Ferrum should report that honestly rather than label the whole session isolated.

The first implementation milestone could cover native model tools, `bash`, and `wait`. Later MCP configuration could assign a containment profile per server.

User-invoked `!` and `!!` commands are different from model-issued tool calls. Their default behavior needs an explicit product decision: follow the active containment profile for predictability, or require the user to select host execution deliberately. It should never change silently based on model output.

## Failure and audit behavior

Containment must fail closed. If the user requests `workspace` isolation and the kernel or backend cannot provide it, Ferrum should reject the operation rather than fall back to host execution.

The effective state should be visible in status output and session metadata:

- requested and effective profile;
- isolation backend and detected capabilities;
- readable and writable roots;
- `.git` mode;
- network mode;
- process-cleanup backend;
- any explicitly enabled host-authority MCP servers.

Project policy may narrow those settings but must not broaden user-configured authority. The model must not be able to change profiles or approve a fallback.

## Regression tests

A containment suite should execute real hostile helpers rather than only inspect command strings. Tests should verify that a child cannot:

- read a canary outside the approved roots;
- read the parent process environment or inherited descriptors;
- write outside the workspace or through symlink escapes;
- modify `.git` in read-only profiles;
- connect through disallowed TCP, Unix, or inherited sockets;
- discover host processes through `/proc`;
- retain a daemon after cancellation, timeout, or normal tool completion.

The same payload should be exercised directly, from a shell script, from an interpreter, and from a build script. Unsupported-kernel and missing-backend tests must verify fail-closed behavior.

## Limits of the proposal

Containment reduces authority; it does not make generated code trustworthy.

- A writable workspace can still be deleted or corrupted.
- Files intentionally readable by the model may still be sent to the configured provider.
- Full network mode permits many forms of exfiltration.
- Modified source may become dangerous when a human later executes it outside containment.
- Kernel, sandbox-backend, compiler, and toolchain vulnerabilities remain possible.
- A user-selected host profile still grants normal user authority.

These limits should remain explicit in the UI and documentation.

## Possible implementation sequence

This is an exploratory order, not a commitment:

1. Separate the current process-cleanup label from execution-isolation status.
2. Define profile semantics, capability detection, and fail-closed configuration.
3. Introduce a credential-free tool worker and move native filesystem tools plus Bash execution behind it.
4. Implement `inspect` with a read-only checkout, private environment, and no network.
5. Add `workspace` with controlled writable paths, read-only `.git`, and private caches.
6. Add explicit network modes and per-MCP-server authority reporting.
7. Explore copy-on-write workspaces and reviewed change application.

The important architectural choice is the boundary itself: the provider-facing process keeps credentials, while model-controlled code executes in a kernel-restricted worker. Everything else can evolve behind that boundary without pretending that more shell-text recognition is a sandbox.

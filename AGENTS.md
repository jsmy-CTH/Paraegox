# Paraegox Agent Rules

Paraegox is a distributed embodied-intelligence Agent OS. Keep that product direction while delivering it through small, executable vertical slices.

## Source and execution authority

- `main` on `https://github.com/jsmy-CTH/Paraegox` is the only integration authority.
- After repository bootstrap, source development, formatting, compilation, and tests run in the designated Ubuntu compute container. The shared server worktree is `/work/Paraegox`.
- The RDP desktop is an editing surface. Rust commands run in the compute container, whose toolchain is under `/root/.cargo/bin`.
- Do not run project build or test commands on the Mac checkout unless the user explicitly changes this policy.
- Never place credentials, model keys, device secrets, tunnel keys, or passwords in the repository, argv, logs, fixtures, or documentation.

## Scope authority and stop rule

- The user's current request is the complete authorized scope. Do not infer permission for the next milestone, adjacent features, speculative hardening, cleanup, refactoring, or future-proofing.
- When the requested outcome is implemented and validated, stop. Report possible next work without starting it unless the user explicitly asks for it.
- Questions, brainstorming, historical documents, architecture diagrams, and old repository code are context, not implementation authorization.
- Do not add a new crate unless the user explicitly approves that crate in the current scope. A crate also needs a present compile, deployment, language, lifecycle, or security boundary; file size or an architecture-layer name is not sufficient.
- Before adding a production file, confirm that no current owner can hold the change coherently and that the file is required by the current executable path. Prefer extending an existing file over creating a speculative layer.
- Do not create placeholder directories, empty modules, future protocol versions, unused traits, duplicate models, speculative fixtures, or compatibility shims.
- Keep plans, status, research, checklists, and progress reports in chat by default. Persist them only when the user explicitly requests a repository artifact.
- Every closeout must state any added production files and crates and why each was necessary. If none were added, say so.

## Development discipline

- Preserve one executable product path at every milestone. Infrastructure without a same-slice producer, consumer, and observable scenario does not merge.
- Do not create crates, services, protocols, registries, journals, compatibility layers, or public abstractions merely to mirror an architecture diagram.
- Add a cross-process or cross-language wire contract only when a real boundary consumes it in the same slice.
- Add durable state only when a named fact must survive a demonstrated process or node failure.
- Keep physical effects behind explicit authority, fencing, and local safety. Never automatically replay an effect whose outcome is unknown.
- Keep queues, retries, recursion, fan-out, and concurrency bounded.
- Old ParaEGOX ADRs and plans are historical inputs, not authority in this repository. Port behavior or code only after checking its current producer, consumer, failure semantics, tests, and license attribution.
- A capability is implemented only when it is present on `main` and its user-visible path passes on the server. Documents, mocks, fixtures, branches, and narrow unit tests alone are not completion evidence.
- Use short-lived feature branches and merge reviewed, validated work into `main`. Do not create permanent validation-snapshot branch families.

## Current validation

Run from `/work/Paraegox` in the Ubuntu compute container:

```bash
export PATH="/root/.cargo/bin:$PATH"
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo run --locked --bin paraegox -- --help
uv sync --project clients/textual --locked
uv run --project clients/textual --locked python -m unittest discover -s clients/textual -p 'test_*.py'
```

Add language- or hardware-specific gates only together with the first real consumer of that toolchain or device boundary.

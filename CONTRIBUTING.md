# Contributing to Paraegox

## Development host

All source development and validation take place on the designated Ubuntu server. The current setup has two relevant containers:

- the RDP sidecar provides the graphical editing desktop;
- the compute container owns compilation and exposes the GPU/toolchain environment.

Both see the shared `/work` directory. Keep the repository at `/work/Paraegox`. Run Rust commands in the compute container, not in the RDP sidecar or on the Mac checkout.

The compute container currently pins Rust through `rust-toolchain.toml`. For non-interactive SSH commands, expose the installed toolchain explicitly:

```bash
export PATH="/root/.cargo/bin:$PATH"
rustup show active-toolchain
```

## Change flow

1. Update `main` from GitHub in `/work/Paraegox`.
2. Create one short-lived branch for one observable product change.
3. Implement the narrowest complete producer-to-consumer path.
4. Run the repository checks in `AGENTS.md` inside the compute container.
5. Commit and push the exact tested revision, then merge it into `main` through review.
6. Remove the feature branch after merge. Build artifacts and validation snapshots are not long-lived source branches.

Do not copy directories wholesale from the legacy repositories. Reuse begins with a behavior and test, followed by the smallest implementation that satisfies it. Preserve applicable license and attribution whenever source is copied.

## Definition of done

A change is done only when the same Git revision contains its implementation, real producer, real consumer, failure behavior, and user-visible or system-level validation. Planning documents and component-only tests do not establish a running capability.

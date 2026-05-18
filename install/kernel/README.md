# crabcc agent kernel

Minimal ARM64 Linux images for Apple Containers / Virtualization.framework,
plus an optional **bleeding-edge** profile for local experimentation.

## Stable profile (6.6 LTS)

```bash
install/kernel/build.sh
# → install/kernel/vmlinuz
```

Uses `config.fragment` — VirtIO, cgroups, THP, no USB/GPU/sound.

## Bleeding-edge profile (6.12+ / mainline)

For developers who want io_uring, BPF, and newer MM tunables on Apple Silicon
containers or bare-metal ARM64 lab hosts:

```bash
LINUX_VERSION=6.12.20 \
  install/kernel/build.sh install/kernel/config.bleeding-edge.fragment
```

The second argument overrides the default `config.fragment` merge.

### What bleeding-edge enables

| Option | Why |
|--------|-----|
| `CONFIG_IO_URING` | Lower syscall overhead for agent I/O loops |
| `CONFIG_BPF`, `CONFIG_BPF_SYSCALL` | eBPF hooks for future index-in-kernel experiments |
| `CONFIG_FTRACE`, `CONFIG_KPROBES` | Latency profiling on agent hot paths |
| `CONFIG_BTRFS_FS` | CoW snapshots of `.crabcc/` index dirs |

**Not production defaults** — larger attack surface and longer compile times.
Use the stable 6.6 fragment for shipped agent containers.

## Custom distro / pet kernel

1. Fork this directory into your distro's kernel packaging tree.
2. Merge `config.fragment` or `config.bleeding-edge.fragment` into your defconfig.
3. Point `CRABCC_KERNEL=vmlinuz` at the built image in your compose/VM spec.

Future work (tracked separately): eBPF program to mmap `.crabcc/index.db` read-only
from the guest without FUSE — requires `CONFIG_BPF_LSM` + userspace loader.

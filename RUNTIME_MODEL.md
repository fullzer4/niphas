# Niphas -- Runtime Model

How a Nix package runs inside a Kubernetes pod without a container image.

## The core insight

A Nix closure is already more self-contained than an OCI image.
Every binary, every library, every data file lives at absolute
`/nix/store/<hash>-<name>-<version>/` paths. No `/usr/lib`, no `/lib`,
no FHS paths. The closure contains everything needed to run the program.

The OCI image is unnecessary. Niphas mounts the closure directly.

## How Nix makes binaries self-contained

### patchelf: the foundation

When Nix builds a package, `patchelf` rewrites two ELF header fields:

**1. PT_INTERP (the dynamic linker path)**

Standard Linux binary:
```
/lib64/ld-linux-x86-64.so.2
```

Nix-built binary:
```
/nix/store/4nlgxhb09sdr51nc9hdm8az5b08vzkgx-glibc-2.38/lib/ld-linux-x86-64.so.2
```

**2. DT_RUNPATH (shared library search path)**

Standard Linux binary:
```
(empty, uses /lib, /usr/lib, ld.so.cache)
```

Nix-built binary:
```
/nix/store/abc123-openssl-3.1/lib:/nix/store/def456-zlib-1.3/lib
```

The dynamic linker finds every `.so` via absolute paths. No system
library search. No `/etc/ld.so.cache`. No `LD_LIBRARY_PATH` needed.

This happens automatically in stdenv's `postFixup` phase:
```bash
patchelf --set-interpreter "$(cat $NIX_CC/nix-support/dynamic-linker)" \
         --set-rpath "${lib.makeLibraryPath buildInputs}" \
         $out/bin/myprogram
```

### Closures: transitive dependency completeness

After building, Nix scans the output for hash references to compute
runtime dependencies. If the binary contains the string
`4nlgxhb09sdr51nc9hdm8az5b08vzkgx` (the hash portion of a glibc store
path), that store path is marked as a runtime dependency. This is
recursive: each dependency's own references are included.

The result is the **closure** -- every store path transitively referenced
by the root package. For a typical application:

```
/nix/store/abc123-myapp-1.0.0         # the application
/nix/store/def456-glibc-2.38          # C library + dynamic linker
/nix/store/ghi789-openssl-3.1         # TLS
/nix/store/jkl012-zlib-1.3            # compression
/nix/store/mno345-gcc-libs-13.2       # C++ runtime
/nix/store/pqr678-ca-certificates     # root CAs
...                                    # every transitive dependency
```

**Invariant**: copying the closure to another machine is sufficient to
run the program. No package manager, no install step, no dependency
resolution at runtime.

### Static (musl) builds

For `pkgsStatic` builds (musl libc, static linking):

- No PT_INTERP (kernel loads the binary directly)
- No DT_RUNPATH (no shared libraries)
- Closure is just the single binary (+ data files if any)
- The simplest case: even `FROM scratch` works

### patchShebangs

Scripts get the same treatment. During `fixupPhase`, Nix rewrites:

```
#!/usr/bin/env python3    -->    #!/nix/store/<hash>-python3-3.11/bin/python3
#!/bin/sh                 -->    #!/nix/store/<hash>-bash-5.2/bin/sh
```

No script references FHS paths after a Nix build.

## How it runs in Kubernetes

### The problem: K8s requires an image

Every container in a Pod spec must have an `image` field. The API server
rejects pods without it. There is no escape from this requirement.

### The solution: stub image + CSI volume

```
+---------------------------------------------+
|  Pod                                        |
|                                             |
|  container:                                 |
|    image: niphas-runner:latest  (~1 MB)     |
|    command: ["/nix/store/abc123-.../bin/app"]|
|                                             |
|  volumes:                                   |
|    - csi:                                   |
|        driver: niphas.io.csi                |
|        /nix/store  (bind mount, read-only)  |
|             |                               |
+-------------|-------------------------------+
              |
              v
  /var/lib/niphas/cache/abc123-myapp-1.0.0/
  /var/lib/niphas/cache/def456-glibc-2.38/
  /var/lib/niphas/cache/ghi789-openssl-3.1/
  ...
```

The stub image satisfies K8s. The actual binary comes from the CSI volume.

### The stub image: niphas-runner

`niphas-runner` is a scratch-based OCI image (~1 MB) containing:

```
/etc/passwd       # root:x:0:0:root:/root:/sbin/nologin
                  # nobody:x:65534:65534:nobody:/:/sbin/nologin
/etc/group        # root:x:0:
                  # nobody:x:65534:
/etc/nsswitch.conf  # passwd: files
                    # group:  files
                    # hosts:  files dns
/tmp/             # writable directory for programs that need it
```

That's it. No shell. No package manager. No libc. No dynamic linker.

**Why no dynamic linker in the stub?** Because the Nix-built binary's
PT_INTERP already points to `/nix/store/<hash>-glibc-2.38/lib/ld-linux-x86-64.so.2`,
which is inside the CSI-mounted closure. The binary finds its own linker.

**For musl-static binaries**: even the stub is overkill. A true `FROM scratch`
(0 bytes) image works because the binary has no PT_INTERP and the kernel
loads it directly.

### The CSI volume: mounting the closure

Niphas uses CSI inline ephemeral volumes (GA since K8s 1.25):

```yaml
volumes:
  - name: nix-closure
    csi:
      driver: niphas.io.csi
      volumeAttributes:
        closurePaths: "/nix/store/abc123-myapp-1.0.0,/nix/store/def456-glibc-2.38,..."
```

No PersistentVolume, no PersistentVolumeClaim, no StorageClass. The volume
lifecycle is tied to the pod.

**Timing guarantee**: kubelet calls `NodePublishVolume` on the CSI driver
and waits for success *before* starting any container. There is no race
condition. The closure is fully mounted when the process starts.

### Execution sequence

```
[1] kubelet reads pod spec, sees CSI volume
         |
         v
[2] kubelet calls NodePublishVolume on niphas-csi
         |
         v
[3] niphas-csi checks local cache at /var/lib/niphas/cache/
    for each store path in closurePaths:
      - if cached: skip (content-addressed, always valid)
      - if not cached: fetch via fallback chain:
          mesh peers (LAN) -> binary cache (WAN)
      - verify Ed25519 signature
      - extract NAR to cache
         |
         v
[4] niphas-csi bind-mounts the cache directory at the target path
    mount(source, target, NULL,
          MS_BIND | MS_RDONLY | MS_NOSUID | MS_NODEV, NULL)
         |
         v
[5] kubelet sees NodePublishVolume returned OK
    starts the container
         |
         v
[6] container runtime (containerd/CRI-O):
    - creates rootfs from stub image (basically empty)
    - adds /nix/store bind mount from CSI volume
    - adds kubelet-managed mounts: /etc/resolv.conf, /etc/hosts, /proc, /sys
    - execs the command: /nix/store/abc123-myapp-1.0.0/bin/myapp
         |
         v
[7] kernel loads the ELF binary
    reads PT_INTERP: /nix/store/def456-glibc-2.38/lib/ld-linux-x86-64.so.2
    this file exists (it's in the mounted closure)
         |
         v
[8] dynamic linker (ld-linux) starts
    reads DT_RUNPATH from the binary
    loads all shared libraries from /nix/store/... paths
    all exist (they're in the closure)
         |
         v
[9] program runs
```

For static (musl) binaries, steps 7-8 simplify: the kernel loads
the binary directly, no dynamic linker involved.

## What kubelet provides automatically

These are mounted into every container by the container runtime,
independent of the image or CSI volumes:

| Path | Source | Writable |
|------|--------|----------|
| `/proc` | procfs | read-only (mostly) |
| `/sys` | sysfs | read-only |
| `/dev` | devtmpfs | device-specific |
| `/etc/resolv.conf` | kubelet (from node or pod dnsConfig) | no |
| `/etc/hosts` | kubelet (from pod spec + hostAliases) | no |
| `/etc/hostname` | kubelet | no |
| `/dev/termination-log` | kubelet | yes |

Niphas does not need to provide any of these.

## What the stub image provides

| Path | Why |
|------|-----|
| `/etc/passwd` | `getpwuid()` calls (some programs check user identity) |
| `/etc/group` | `getgrgid()` calls |
| `/etc/nsswitch.conf` | tells glibc's NSS where to look for users/hosts |
| `/tmp/` | writable dir for programs that need temporary files |

The operator also injects an `emptyDir` volume at `/tmp` in the generated
Deployment spec for programs that need writable temp space.

## NSS and dlopen

glibc uses `dlopen()` to load NSS modules (`libnss_files.so`,
`libnss_dns.so`) at runtime. This is a well-known challenge for Nix.

**How it works in Niphas:**

1. The Nix-patched glibc searches `$NIX_GLIBC_NSS_PATH` (a Nix extension)
   for NSS modules
2. Basic NSS modules (`files`, `dns`) are shipped with glibc itself and
   are included in every closure that depends on glibc
3. The stub image's `/etc/nsswitch.conf` restricts lookups to `files dns`,
   which are the modules available in the closure

For most server applications (HTTP APIs, databases, CLI tools), this is
sufficient. Complex NSS configurations (LDAP, SSSD) would require
additional packages in the closure.

## Edge cases

### Programs that hardcode /bin/sh

C programs calling `system()` exec `/bin/sh`. Two solutions:

1. **Package-level fix**: patch the source to use `execvp("sh", ...)`
   instead of `system()` (the proper Nix approach)
2. **Stub image fallback**: include `/bin/sh` as a symlink to the
   bash in the closure. Niphas can do this if `meta.needsShell = true`

Well-packaged nixpkgs derivations already handle this via
`patchShebangs` and wrapper scripts.

### GPU workloads (NVIDIA)

GPU drivers have two components:

- **Kernel modules** (`nvidia.ko`): run on the host, loaded by the
  node's OS. Not part of any container.
- **Userspace libraries** (`libcuda.so`): must match the kernel module
  version exactly.

Niphas handles GPUs the same way as every other K8s storage driver:

1. NVIDIA device plugin runs as a DaemonSet on GPU nodes
2. The device plugin injects userspace driver libraries via a volume mount
3. `LD_LIBRARY_PATH` is set to include the injected driver path
4. The Nix closure does not include GPU drivers (they're host-specific)

This is the standard Kubernetes NVIDIA pattern. Niphas doesn't change it.

### setuid binaries

The Nix store cannot contain setuid binaries (security invariant). The
CSI volume is mounted with `MS_NOSUID`, so even if a setuid bit existed
it would not function.

Programs that need elevated capabilities (e.g., `ping` needs
`CAP_NET_RAW`) should use Kubernetes `securityContext.capabilities.add`
instead of filesystem setuid bits.

### dlopen for plugins

Programs that `dlopen("libplugin.so")` by relative name need help
finding the library. Nix handles this via:

1. **Patching dlopen calls**: the nixpkgs package patches source code
   to use absolute `/nix/store/...` paths (most common approach)
2. **wrapProgram**: sets `LD_LIBRARY_PATH` in a wrapper script
3. **CRD env field**: the user can set `LD_LIBRARY_PATH` in
   `spec.env` to include plugin paths from the closure

Well-maintained nixpkgs packages already have dlopen paths patched.

## Comparison with traditional containers

| Aspect | OCI Container | Niphas (Nix closure) |
|--------|--------------|----------------------|
| **Image build** | Dockerfile, layer by layer | `nix build`, content-addressed |
| **Image size** | 50 MB - 2 GB typical | Closure: 50-500 MB, stub: ~1 MB |
| **Deduplication** | Per-layer (coarse, 256 max) | Per-store-path (fine-grained, unlimited) |
| **Reproducibility** | Best-effort (apt install = non-deterministic) | Guaranteed (same inputs = same hash) |
| **Registry** | Required (Docker Hub, ECR, GCR) | Not needed (binary cache serves NARs) |
| **Pull time** | Download all layers | Download only missing store paths |
| **Update granularity** | Re-push entire changed layers | Only new/changed store paths |
| **Rollback** | Re-tag or re-pull old image | Switch store path (atomic) |
| **Runtime deps** | Implicit (whatever is in the image) | Explicit (closure is the complete set) |
| **Security scanning** | Scan image layers for CVEs | Scan closure for CVEs (more precise) |

## Why not just use dockerTools?

nixpkgs provides `dockerTools.buildLayeredImage` which converts a Nix
closure into OCI layers. This works, but:

1. **Adds a build step**: `nix build` -> OCI image -> push to registry -> pull
2. **Loses deduplication**: OCI layers are coarser than Nix store paths.
   Two images sharing glibc but built at different times get different
   layers (different hashes due to layer ordering)
3. **Requires a registry**: adds infrastructure dependency
4. **Layer limit**: OCI spec allows 128 layers. A closure with 300+ store
   paths must merge some into fat layers, losing granularity
5. **Redundant wrapping**: the OCI image is just the closure in a different
   format. Mounting the closure directly is more efficient

Niphas skips the middleman. The binary cache *is* the registry.
The closure *is* the image. The CSI driver *is* the image puller.

## Summary

```
Nix build        patchelf        closure         binary cache
  |                |               |                |
  v                v               v                v
source code -> ELF binary -> all store paths -> NARs on HTTP
               (absolute       (glibc, ssl,      (content-
                /nix/store      zlib, ...)         addressed)
                paths)

                   CSI driver           kubelet
                      |                   |
                      v                   v
   fetch NARs -> extract to cache -> bind mount -> exec binary
                 /var/lib/niphas/    /nix/store    /nix/store/
                 cache/              (read-only)   <hash>/bin/app
```

The entire chain is content-addressed. Same inputs produce the same
store path hash. Same hash fetches the same NAR. Same NAR extracts
the same files. Reproducible from source to running process.

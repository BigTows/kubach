<p align="center">
  <img src="crates/kubach-app/assets/icon/kubach-512.png" width="128" alt="Kubach">
</p>

<h1 align="center">Kubach</h1>

<p align="center">
  <b>A native, GPU-accelerated Kubernetes IDE — with a safe door for your AI agent.</b>
</p>

<p align="center">
  <a href="https://github.com/BigTows/kubach/releases"><img src="https://img.shields.io/github/v/release/BigTows/kubach?label=release" alt="Release"></a>
  <img src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-x64%20%26%20arm64-informational" alt="Platforms">
  <img src="https://img.shields.io/badge/built%20with-Rust%20%2B%20GPUI-orange" alt="Rust + GPUI">
  <img src="https://img.shields.io/badge/vibe-coded%20%F0%9F%A4%96-blueviolet" alt="Vibe-coded">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="MIT"></a>
</p>

<p align="center">
  <b>English</b> · <a href="README_RU.md">Русский</a> · <a href="README_CN.md">简体中文</a>
</p>

---

> [!NOTE]
> **Kubach is a vibe-coded project.** Nearly all of its code was written by AI agents,
> with a human setting the direction, reviewing the result and making the calls. We say so up front so that you know what you're running.

## Why Kubach

- **Native and fast.** Written in Rust on [GPUI](https://gpui.rs), the GPU UI framework
  behind the Zed editor. No Electron and no browser inside.
- **Nothing goes into your cluster.** Kubach runs on your machine with the rights
  from your kubeconfig. It has no in-cluster agent, no operator and no CRDs.
- **Built for working with AI.** Kubach is also an MCP server. Claude Code, Claude
  Desktop or any other MCP client can work with your cluster within limits you set.
  The agent never sees your credentials.
- **Your kubeconfigs stay safe.** Imported configs live in an encrypted vault that
  unlocks with a password, **Touch ID** or **Windows Hello**, and locks with your screen.

## Features

**Explore**
- Several clusters at once: switch contexts instantly, connections stay warm.
- Every resource kind, CRDs included, with live list + watch updates, events and
  diagnostics.
- **Topology**: a graph from Ingress / Gateway API to Service, workloads and pods, down
  to ConfigMaps, Secrets and PVCs. It flags broken references and checks
  NetworkPolicies ("can A reach B on port 443?").
- Traffic overlay from Prometheus for Istio, Linkerd and Cilium Hubble.
- CPU and memory from metrics-server, and history from Prometheus.

**Operate**
- Logs, exec, port-forward, debug containers and a shell on the node.
- Scale, rollout restart, cordon / drain and evictions.
- A YAML editor with syntax highlighting, a diff before you apply, and server-side apply.
- Secrets that stay secret: values are fetched only when you click "Show", never
  cached, and cleared from the clipboard on a timer.
- A built-in terminal with a `KUBECONFIG` scoped to the context you're in.

**Ship**
- **Helm**: releases, history, upgrade, rollback, chart repositories with icons and
  READMEs, and OCI charts. Kubach can download a pinned, checksum-verified `helm` for you.
- **Kustomize**: build and diff overlays before applying.
- **GitOps**: shows which Argo CD Application or Flux Kustomization / HelmRelease
  manages a resource.

**Collaborate with AI** ([MCP](docs/guide.md#mcp-ии-агенты))
- Sessions are scoped to a context, namespaces and a profile: `read-only`, `debug` or
  `deploy`.
- Rights are granted on request: the agent asks, and you answer yes or no in the
  Kubach window.
- Writes, exec and Secret reveals each need your confirmation, or can be limited to
  dry run and diff.
- Every call goes to an audit log, which never contains Secret values.
- Topology tools for the agent: `get_topology` and `can_reach`.
- Connects over stdio (`kubach mcp`) or HTTP on `127.0.0.1` with a bearer token.

**Feels like home**
- Kubach Dark and Kubach Light themes.
- Interface in English, Russian and Simplified Chinese.

## Install

Download the latest build from **[Releases](https://github.com/BigTows/kubach/releases)**.
Each build is available for both x64 and arm64.

| OS | Package | Notes |
|---|---|---|
| **macOS** 11+ | `Kubach-<version>-macos-<arch>.dmg` | Signed with Developer ID and notarized |
| **Windows** 10+ | `.msi` installer or portable `.zip` | Not code-signed yet: SmartScreen may show a warning |
| **Debian / Ubuntu** | `.deb` | `sudo apt install ./kubach_*.deb` |
| **Fedora / RHEL / openSUSE** | `.rpm` | `sudo dnf install ./kubach-*.rpm` |
| **Arch Linux** | `.pkg.tar.zst` or `PKGBUILD` (`kubach-bin`) | `sudo pacman -U kubach-*.pkg.tar.zst` |
| **Any Linux** | `.AppImage` or `.tar.gz` | Needs glibc 2.35+ and a Vulkan driver |

Every push to `main` also produces fresh builds as
[Actions artifacts](https://github.com/BigTows/kubach/actions/workflows/build.yml).

### Connect your AI agent

Agent access is on by default for every context. Without asking, the agent may only
read built-in resources (pods, deployments, services, events…) in any namespace; no
Secrets. Anything more (logs and exec, changes, Secrets, custom resources) it has to
ask for, and you answer in the Kubach window: for this session, or with
**Allow by default** to raise the context's minimal access for good. Changes, deletions
and every exec command are still confirmed one by one, and nothing exceeds your own
RBAC. Turn access off, or set or reset the minimal access, per context in
**Settings → Security**.

Kubach can register itself with your agent. It shows the config file and the change,
applies them after you confirm, and keeps your other servers along with a backup of
the file:

```sh
kubach mcp install claude-code      # also: codex, opencode, cursor, claude-desktop
kubach mcp clients                  # which agents are connected
kubach mcp uninstall cursor
```

You can also run the stdio server yourself and tune the session:

```sh
kubach mcp                                   # the agent picks a context; Kubach asks you first
kubach mcp --context prod --namespace shop   # pin a context and a namespace (repeat -n for more)
kubach mcp -c staging -p read-only --ttl 2h  # never more than reading, end after 2 hours
kubach mcp --help
```

| Option | Meaning |
|---|---|
| `-c, --context NAME` | kubeconfig context (without any option, the agent chooses and you approve) |
| `-n, --namespace NS` | limit the session to a namespace; repeat for several |
| `-p, --profile` | `read-only`, `debug` or `deploy`: a cap for this process; requests above it are refused without asking |
| `--ttl` | session length: `30m`, `2h`, `1h30m` (8 hours by default and at most) |
| `--grant-read` | kept for compatibility; reading is always part of the minimal access |

Access requests and changes are confirmed in the Kubach window, so keep the app running
with **Settings → AI agent → HTTP server** turned on. The same HTTP server can serve
agents directly at `http://127.0.0.1:<port>/mcp` with a bearer token. Ready-made
commands with your path, port and token are under **Settings → AI agent → Connect an agent**.

Where the binary lives: `/Applications/Kubach.app/Contents/MacOS/kubach` on macOS,
`/usr/bin/kubach` on Linux packages, `C:\Program Files\Kubach\kubach.exe` on Windows.

## Build from source

You need a recent stable Rust toolchain plus the platform's GPUI prerequisites: Xcode
on macOS, the Visual Studio C++ build tools on Windows, and on Linux the X11/Wayland,
xkbcommon, Vulkan and fontconfig dev packages.

```sh
cargo run --release                        # the app
cargo run -p kubach-ui --example gallery   # design system gallery
```

The [guide](docs/guide.md) covers platform details, the kubeconfig vault, MCP
configuration and packaging (in Russian for now).

## Learn more

- [Guide](docs/guide.md): building, kubeconfig vault, MCP setup.
- [Architecture](docs/architecture.md) and [decision records](docs/adr/README.md).
- [Project rules](AGENT.md): the conventions the agents follow.

Versions follow `YEAR.MONTH.INC`, for example `2026.10.1`.

## License

[MIT](LICENSE)

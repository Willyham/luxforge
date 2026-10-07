# Platforms

Accepted target scope with provisional engineering baselines. These are intended support and testing boundaries, not verified compatibility.

| Target | Baseline | Artifact | Graphics and windowing | Verification status |
| --- | --- | --- | --- | --- |
| `aarch64-apple-darwin` | macOS 14 or later | `.app` in an unsigned ZIP | Metal, AppKit | Verified natively on the owner's M4 Pro (macOS 26.5.2); macOS 14 floor unverified |
| `x86_64-pc-windows-msvc` | Windows 11 24H2 or later | Executable and notices in a ZIP | wgpu DX12, native Win32 picker | CI disabled; Windows support will come later; native desktop check deferred, no machine available |
| `x86_64-unknown-linux-gnu` | Ubuntu 24.04 LTS, glibc 2.39 | Directory in a `.tar.gz` | Vulkan; GNOME Wayland primary, X11 required | Hosted build/package and prior no-adapter reference/owner checks; current refresh blocked by an index-watcher failure and the software Vulkan rendered lane's photo-sized deadline ([CI](development.md#ci)); native desktop and GPU checks deferred |

Module secrets use the macOS Keychain, verified natively by an opt-in test on the M4. Windows Credential Manager and Linux Secret Service are not implemented: there, every secret method answers `not-ready` and nothing is stored in plain text, so a module that needs a secret cannot be configured yet. The module transport verifies TLS with the operating system's verifier on macOS and Windows and native roots on Linux; only macOS has been exercised.

A build on a newer SDK does not prove execution on the floor. Desktop results must name the actual OS, architecture and backend. Linux ARM64 or Windows ARM emulation may supplement the x64 matrix, never replace it. The owner's Omarchy VM runs ARM64 Arch Linux (kernel 7.2.6) with Rust 1.94.0. Focused core job-monitoring tests pass there: bounded waits and cancellation, disconnect cleanup and response ordering, and progress/terminal publication. These are VM functional checks, not Linux desktop or GPU acceptance. Native minimize/hide visibility facts remain unsupported on Linux, so minimizing a normal editor window does not pause UI sampling there yet.

## Prerequisites

- All hosts: Rust 1.94.0 with rustfmt and Clippy, plus Git. Tooling is Rust-only.
- macOS: Xcode Command Line Tools, Metal-capable Apple Silicon and an unlocked graphical desktop for interaction checks. Bundles are unsigned and not notarized.
- Windows: Visual Studio Build Tools with the C++ toolchain and Windows SDK, a DX12-capable driver and a desktop session. Inspect the artifact's runtime imports before calling packaging self-contained.
- Linux: `build-essential pkg-config libx11-dev libxkbcommon-dev libwayland-dev libvulkan-dev` to build; `libxkbcommon-x11-0` (not supplied by `libxkbcommon-dev`), a Vulkan driver and a working XDG desktop portal with a file chooser backend at runtime. Build on Ubuntu 24.04 itself to avoid raising the glibc baseline. Headless smoke additionally needs Xvfb and Mesa Vulkan drivers.

The `rfd` configuration uses the XDG portal rather than GTK bindings; an absent portal is a runtime file-picker failure, not proof of a working app. Software Vulkan supports functional CI and is labelled as such, never used as GPU performance evidence: every rendered run records the adapter that drew it, whose device type `Cpu` names a software rasterizer. A machine with no graphics adapter runs the reference renderer's tests and `luxforge-json`, but the editor cannot open its window there.

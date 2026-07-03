## Installing & building

### System requirements

| Requirement                 | Details                                                         |
| --------------------------- | --------------------------------------------------------------- |
| Operating systems           | macOS 12+, Ubuntu 20.04+/Debian 10+, or Windows 11 **via WSL2** |
| Git (optional, recommended) | 2.23+ for built-in PR helpers                                   |
| RAM                         | 4-GB minimum (8-GB recommended)                                 |

### DotSlash

GitHub Releases also contain a [DotSlash](https://dotslash-cli.com/) shim named `code`. Checking the DotSlash file into your repo pins contributors to the same binary across platforms.

### Build from source

```bash
# Clone the repository and navigate to the workspace root.
git clone https://github.com/cbusillo/code.git
cd code

# Install the Rust toolchain, if necessary.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Build everything (CLI, TUI, MCP servers). This is the same check CI runs.
./build-fast.sh

# Preview PATH/release-bin wiring, then install the PATH-resolved local binary.
just local-code-rebuild-preflight
just local-code-rebuild
code -- "explain this codebase to me"
```

> [!NOTE]
> The project treats compiler warnings as errors. The only required local check is `./build-fast.sh`; skip `rustfmt`/`clippy` unless asked.

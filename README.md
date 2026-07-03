<img src="docs/images/every-logo.png" alt="Every Code Logo" width="400">

&ensp;

**Every Code** (Code for short) is a fast, local coding agent for your terminal.
It uses Codex CLI as the upstream substrate and layers Every Code features,
defaults, releases, and UX on top where they make the product better.

&ensp;
## What's new

- **Codex CLI substrate alignment** – current development keeps the editable
  Rust product workspace in `code-rs` close to upstream Codex CLI, with Every
  Code behavior added as small product-layer overlays.
- **Code Bridge** – Sentry-style local bridge that streams errors, console,
  screenshots, and control from running apps into Every Code.
- **Multi-agent commands** – `/plan`, `/solve`, and `/code` coordinate multiple
  CLI agents while preserving Codex-shaped execution and session primitives.
- **Goal-mode direction** – standalone Auto Drive is retired; useful
  long-running-work behavior should attach to current goal, review, guardian,
  session, and agent surfaces.

 [Read the full notes in RELEASE_NOTES.md](docs/release-notes/RELEASE_NOTES.md)

&ensp;
## Why Every Code

- 🚀 **Codex-aligned goal mode** – Long-running work builds on upstream-shaped
  goal, review, session, and agent primitives.
- 🌐 **Browser Integration** – CDP support, headless browsing, screenshots captured inline.
- 🤖 **Multi-agent commands** – `/plan`, `/code` and `/solve` coordinate multiple CLI agents.
- 🧭 **Unified settings hub** – `/settings` overlay for limits, theming, approvals, and provider wiring.
- 🎨 **Theme system** – Switch between accessible presets, customize accents, and preview live via `/themes`.
- 🔌 **MCP support** – Extend with filesystem, DBs, APIs, or your own tools.
- 🔒 **Safety modes** – Read-only, approvals, and workspace sandboxing.

&ensp;
## AI Videos

&ensp;
<p align="center">
  <a href="https://www.youtube.com/watch?v=Ra3q8IVpIOc">
    <img src="docs/images/video-auto-review-play.jpg" alt="Play Auto Review video" width="100%">
  </a><br>
  <strong>Auto Review</strong>
</p>

&ensp;
<p align="center">
  <a href="https://youtu.be/sV317OhiysQ">
    <img src="docs/images/video-v03-play.jpg" alt="Play Multi-Agent Support video" width="100%">
  </a><br>
  <strong>Multi-Agent Promo</strong>
</p>



&ensp;
## Quickstart

### Run

```bash
code
```

### Install & Run

```bash
just local-code-rebuild
code
```

Use `code update-check` to inspect the current GitHub Release manifest and
`code update --yes` to install a newer verified direct binary when one is
available. Every Code releases are distributed through GitHub Releases; npm and
Homebrew publishing are not part of the current release path.

**Authenticate** (one of the following):
- **Sign in with ChatGPT** (Plus/Pro/Team; uses models available to your plan)
  - Run `code` and pick "Sign in with ChatGPT"
- **API key** (usage-based)
  - Set `export OPENAI_API_KEY=xyz` and run `code`

### Install External Agents (optional)

Every Code supports orchestrating other AI CLI tools. Install these and config to use alongside Every Code.

```bash
# Ensure Node.js 20+ is available locally (installs into ~/.n)
npm install -g n
export N_PREFIX="$HOME/.n"
export PATH="$N_PREFIX/bin:$PATH"
n 20.18.1

# Install the companion CLIs
export npm_config_prefix="${npm_config_prefix:-$HOME/.npm-global}"
mkdir -p "$npm_config_prefix/bin"
export PATH="$npm_config_prefix/bin:$PATH"
npm install -g @anthropic-ai/claude-code @google/gemini-cli @qwen-code/qwen-code

# Quick smoke tests
claude --version
gemini --version
qwen --version
```

> ℹ️ Add `export N_PREFIX="$HOME/.n"` and `export PATH="$N_PREFIX/bin:$PATH"` (plus the `npm_config_prefix` bin path) to your shell profile so the CLIs stay on `PATH` in future sessions.

&ensp;
## Commands

### Browser
```bash
# Connect code to external Chrome browser (running CDP)
/chrome        # Connect with auto-detect port
/chrome 9222   # Connect to specific port

# Switch to internal browser mode
/browser       # Use internal headless browser
/browser https://example.com  # Open URL in internal browser
```

### Agents
```bash
# Plan code changes (Claude, Gemini and GPT-5 consensus)
# All agents review task and create a consolidated plan
/plan "Stop the AI from ordering pizza at 3AM"

# Solve complex problems (Claude, Gemini and GPT-5 race)
# Fastest preferred (see https://arxiv.org/abs/2505.17813)
/solve "Why does deleting one user drop the whole database?"

# Write code! (Claude, Gemini and GPT-5 consensus)
# Creates multiple worktrees then implements the optimal solution
/code "Show dark mode when I feel cranky"
```

### General
```bash
# Try a new theme!
/themes

# Change reasoning level
/reasoning low|medium|high

# Switch models or effort presets
/model

# Start new conversation
/new
```

## CLI reference

```shell
code [options] [prompt]

Options:
  --model <name>        Override the model for the active provider (e.g. gpt-5.1)
  --read-only          Prevent file modifications
  --no-approval        Skip approval prompts (use with caution)
  --config <key=val>   Override config values
  --oss                Use local open source models
  --sandbox <mode>     Set sandbox level (read-only, workspace-write, etc.)
  --help              Show help information
  --debug             Log API requests and responses to file
  --version           Show version number
```

Note: `--model` only changes the model name sent to the active provider. To use a different provider, set `model_provider` in `config.toml`. Providers must expose an OpenAI-compatible API (Chat Completions or Responses).

&ensp;
## Memory & project docs

Every Code can remember context across sessions:

1. **Create an `AGENTS.md` or `CLAUDE.md` file** in your project root:
```markdown
# Project Context
This is a React TypeScript application with:
- Authentication via JWT
- PostgreSQL database
- Express.js backend

## Key files:
- `/src/auth/` - Authentication logic
- `/src/api/` - API client code  
- `/server/` - Backend services
```

2. **Session memory**: Every Code maintains conversation history
3. **Codebase analysis**: Automatically understands project structure

&ensp;
## Non-interactive / CI mode

For automation and CI/CD:

```shell
# Run a specific task
code --no-approval "run tests and fix any failures"

# Generate reports
code --read-only "analyze code quality and generate report"

# Batch processing
code --config output_format=json "list all TODO comments"
```

&ensp;
## Model Context Protocol (MCP)

Every Code supports MCP for extended capabilities:

- **File operations**: Advanced file system access
- **Database connections**: Query and modify databases
- **API integrations**: Connect to external services
- **Custom tools**: Build your own extensions

Configure MCP in `~/.code/config.toml` Define each server under a named table like `[mcp_servers.<name>]` (this maps to the JSON `mcpServers` object used by other clients):

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/project"]
```

&ensp;
## Configuration

Main config file: `~/.code/config.toml`

> [!NOTE]
> Every Code reads from both `~/.code/` and `~/.codex/` for backwards compatibility, but it only writes updates to `~/.code/`. If you switch back to Codex and it fails to start, remove `~/.codex/config.toml`. If Every Code appears to miss settings after upgrading, copy your legacy `~/.codex/config.toml` into `~/.code/`.

```toml
# Model settings
model = "gpt-5.1"
model_provider = "openai"

# Behavior
approval_policy = "on-request"  # untrusted | on-failure | on-request | never
model_reasoning_effort = "medium" # low | medium | high
sandbox_mode = "workspace-write"

# UI preferences see THEME_CONFIG.md
[tui.theme]
name = "light-photon"

# Add config for specific models
[profiles.gpt-5]
model = "gpt-5.1"
model_provider = "openai"
approval_policy = "never"
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"
```

### Environment variables

- `CODE_HOME`: Override config directory location
- `OPENAI_API_KEY`: Use API key instead of ChatGPT auth
- `OPENAI_BASE_URL`: Use OpenAI-compatible API endpoints (chat or responses)
- `OPENAI_WIRE_API`: Force the built-in OpenAI provider to use `chat` or `responses` wiring

&ensp;
## FAQ

**How is this different from the original?**
> Every Code uses Codex CLI as the upstream substrate while owning its browser integration, multi-agent commands (`/plan`, `/solve`, `/code`), theme system, Code Bridge, and release defaults.

**Can I use my existing Codex configuration?**
> Yes. Every Code reads from both `~/.code/` (primary) and legacy `~/.codex/` directories. We only write to `~/.code/`, so Codex will keep running if you switch back; copy or remove legacy files if you notice conflicts.

**Does this work with ChatGPT Plus?**
> Absolutely. Use the same "Sign in with ChatGPT" flow as the original.

**Is my data secure?**
> Yes. Authentication stays on your machine, and we don't proxy your credentials or conversations.

&ensp;
## Contributing

We welcome contributions! Every Code accepts upstream improvements when they fit the product, but product direction, defaults, and release quality are decided here.

### Development workflow

```bash
# Clone and setup
git clone https://github.com/cbusillo/code.git
cd code
npm install

# Build (use fast build for development)
./build-fast.sh

# Run locally
./code-rs/target/dev-fast/code
```

#### Git hooks

This repo ships shared hooks under `.githooks/`. To enable them locally:

```bash
git config core.hooksPath .githooks
```

The `pre-push` hook runs `./pre-release.sh` automatically when pushing to `main`.

### Opening a pull request

1. Fork the repository
2. Create a feature branch: `git checkout -b feature/amazing-feature`
3. Make your changes
4. Build successfully: `./build-fast.sh`
5. Submit a pull request


&ensp;
## Legal & Use

### License & attribution
- This project descends from `openai/codex` under **Apache-2.0**. We preserve upstream LICENSE and NOTICE files.
- **Every Code** (Code) is **not** affiliated with, sponsored by, or endorsed by OpenAI.

### Your responsibilities
Using OpenAI, Anthropic or Google services through Every Code means you agree to **their Terms and policies**. In particular:
- **Don't** programmatically scrape/extract content outside intended flows.
- **Don't** bypass or interfere with rate limits, quotas, or safety mitigations.
- Use your **own** account; don't share or rotate accounts to evade limits.
- If you configure other model providers, you're responsible for their terms.

### Privacy
- Your auth file lives at `~/.code/auth.json`
- Inputs/outputs you send to AI providers are handled under their Terms and Privacy Policy; consult those documents (and any org-level data-sharing settings).

### Subject to change
AI providers can change eligibility, limits, models, or authentication flows. Every Code supports **both** ChatGPT sign-in and API-key modes so you can pick what fits (local/hobby vs CI/automation).

&ensp;
## License

Apache 2.0 - See [LICENSE](LICENSE) file for details.

Every Code is an independent coding agent descended from the original Codex CLI. We maintain compatibility where it helps users while developing Every Code's own product direction.

&ensp;
---
**Need help?** Open an issue on [GitHub](https://github.com/cbusillo/code/issues) or check our documentation.

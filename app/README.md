# Blueprint App

The desktop app for the [Blueprint](https://github.com/ajilach/ajila-forms-conversion-engine) project: drop
in the PDF forms, and an autonomous agent converts them into an AEM Adaptive Forms
package or a Redacto document, showing its work as it goes.

Built with [Dioxus](https://dioxuslabs.com/) for macOS, Windows and Linux. The latest
release can be downloaded from the [releases page](https://github.com/ajilach/ajila-forms-conversion-engine/releases).

## Running

```sh
# From the repository root
cargo run --release -p blueprint-app

# Or with hot reloading, from this directory
dx serve --platform desktop
```

An Anthropic API key has to be configured in Settings before a conversion can be
started. Settings are stored in `<config_dir>/blueprint/history.db`, alongside the
edit history and the reference-form store.

## Bundling

```sh
dx bundle --platform desktop
```

## Layout

| Path | Contents |
|---|---|
| `src/main.rs` | App state and the three full-page views. |
| `src/components/` | The agent flow, settings and reference-form manager. |
| `src/agent_runner.rs` | Sequences the Author → Reviewer stages over one agent. |
| `src/llm.rs` | Anthropic Messages-API client: streaming, prompt caching, context budget. |
| `src/files.rs` | Saving artefacts to Downloads and showing them to the user. |
| `design/` | Static HTML mockups. Not part of the build. |

# OpenRouter Operator Guide

OpenRouter is nano's first implemented real provider. The adapter uses the
OpenRouter API at `https://openrouter.ai/api/v1/` and starts with the pinned
model `deepseek/deepseek-v4-flash-0731`.

## Credentials

nano resolves credentials for catalog discovery and each generation in this order:

1. A non-empty `OPENROUTER_API_KEY` environment variable wins, and the auth
   file is not read. An unset or empty variable falls through to the file.
2. On Linux and macOS, an absolute, non-empty `XDG_CONFIG_HOME` selects
   `$XDG_CONFIG_HOME/nano/auth.json`.
3. Otherwise, nano uses `$HOME/.config/nano/auth.json` on those platforms.

Use this OpenRouter credential shape, with a locally supplied key:

```json
{"openrouter":{"type":"api","api_key":"<api-key>"}}
```

The file is optional, read-only from nano's perspective, and re-read for each
generation. nano never writes it and never checks or changes its file mode.
The file credential path is supported on Linux and macOS only. nano on Windows
is unsupported in this phase; environment lookup still occurs before
platform-specific file resolution, but that lookup does not constitute
Windows support. OAuth credentials, including `"type":"oauth"`, are
unsupported.

Missing, malformed, empty, or unsupported credentials produce readable setup
errors without exposing the key. Keep real keys out of documentation, logs,
and automated test environments.

## Runtime Behavior

At startup, nano uses bundled metadata for the default model while it loads
the OpenRouter `/models/user` catalog in the background with an authorization
header when credentials resolve successfully. OpenRouter documents this
authenticated endpoint as filtering by user provider preferences, privacy
settings, and guardrails. If credential resolution fails before discovery, nano
uses the public `/models` catalog without an authorization header so the picker
still works without credentials. A catalog HTTP/API failure does not silently
fall back to the public catalog and does not disable the pinned default. Reopen
`Ctrl-P` to retry discovery after a catalog failure.

While nano is not responding, `Ctrl-P` opens the model picker. Search is
case-insensitive and matches the model name or ID. Rows include the context
limit and input/output prices in `$` per million tokens. The current model is
shown first. `Enter` waits for runtime acknowledgment before closing the
modal; `Esc` closes it. Selection and request errors stay visible; nano does
not silently fall back to another model.

Each generation is one independent request containing one user message. A
second turn does not include previous history. The current request scope has
no tools, multi-turn history, cache controls, session ID, OAuth, or automatic
retries. The HTTP client has a 10-second connect timeout, and dropping a
stream closes the request cleanly.

The status line reports raw `in`, `cache`, and `out` token counts. `cache` is
the cached-input token count, not a computed ratio. A length-limited response
is shown as `truncated: output length limit`.

HTTP and stream failures remain visible as setup or streaming errors. API
messages may include retry timing and a generation ID when supplied; loaded
keys are redacted. Model selection errors direct the operator back to `Ctrl-P`.

The current picker is entered with `Ctrl-P`. A future prompt draft beginning
with `/` may offer `/models` and reuse this picker, but `CommandMenu` parsing
and suggestions are not implemented.

## Manual Smoke

Use a credit-limited OpenRouter key. Successful generations consume provider
credits, so this procedure is manual only and is never part of automated
tests.

Choose one credential setup:

```sh
# Environment option; substitute the key only in your local shell.
OPENROUTER_API_KEY='<api-key>' cargo run
```

Or create the JSON file shown above at
`$XDG_CONFIG_HOME/nano/auth.json` when `XDG_CONFIG_HOME` is absolute and
non-empty, or at `$HOME/.config/nano/auth.json` otherwise. Use a clean shell
or unset a non-empty `OPENROUTER_API_KEY`, because it takes precedence over
the file, then run:

```sh
unset OPENROUTER_API_KEY
cargo run
```

Verify the following:

1. The initial model is `deepseek/deepseek-v4-flash-0731`, and the background
   catalog can populate the picker. If discovery fails, the default remains
   usable.
2. When idle, press `Ctrl-P`, search by part of a model name or ID, and close
   the picker with `Esc` or select a model with `Enter`.
3. Submit a harmless, short prompt such as `Reply with one short line about
   the number 2.` and observe streamed text plus raw `in`, `cache`, and `out`
   usage.
4. Submit a second harmless prompt and confirm it is an independent turn.
5. Optionally, in a separate run, use a deliberately invalid placeholder such
   as `not-a-real-key` and confirm that the authentication error is readable
   and does not echo the credential.
6. Exit with `Ctrl-C` and confirm the terminal restores cleanly.

Normal checks remain credential-free:

```sh
cargo fmt --check && cargo test
```

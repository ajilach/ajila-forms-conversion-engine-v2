# The Redacto platform the verifier boots

`u2s-redacto-ubs-verify-mcp` verifies a `redacto-ubs` dump by importing it
into a Redacto platform and rendering it there. The verifier boots that
platform itself, one per `session_id`, the same way the AEM verifiers boot
their AEM instances. Compose only pulls the images.

## What a session boots

On a session's first `verify_run`, on a network of its own
(`u2s-verify-redacto-<session>-<uuid>`):

| Container | What it does |
|---|---|
| `postgres` | The platform's database (`redacto`), never published, no volume |
| bootstrap | Creates the `app_redacto` role and schema ([bootstrap.sql](../../crates/u2s-redacto-verify-core/sql/bootstrap.sql), vendored from `ajila-redacto-platform`), run through `psql` inside `postgres` |
| `migration` | Applies the platform's Flyway migrations (schema only, no sample data), then is removed |
| `core` | Resolves a document's template from the database |
| `rendering` | Renders it to PDF (`POST /bin/redacto/rendering/integration`) |

Later calls of the same session reuse the platform. A platform unused for
`U2S_REDACTO_VERIFY_UBS_IDLE_TIMEOUT_SECS` is torn down, and a restarted verifier removes whatever a previous process
left behind (every container and network carries the label
`u2s.verify.format=redacto-ubs`). See
`crates/u2s-redacto-verify-core/src/session.rs`.

Each active session costs about 1.5 GB of memory: `core` about 1 GB,
`rendering` about 0.5 GB, Postgres a few dozen MB. A first boot takes
about 15 seconds on a warm host.

## What `verify_run` does

1. Checks the dump offline (it must decode).
2. Boots the session's platform, or reuses it.
3. In one `psql` session inside the platform's Postgres: deletes every row
   that document id already has (its document, versions, ownerships,
   relations, and the assets only it owns), then runs the dump. Re-verifying
   an edited document therefore never collides with its previous import.
   See `crates/u2s-redacto-verify-core/src/platform.rs`.
4. Calls `rendering` once per declared language and returns each PDF.

The session stays locked from step 2 to 4, so a render always sees its own
call's import, and two sessions never see each other's documents.

## Settings

The images are the ones `ajila-redacto-platform`'s CI publishes to
`ajilaclouddev.azurecr.io`, pinned in `.env.example`. Pulling them needs an
`az acr login --name ajilaclouddev` on the host; the verifier itself has no
registry credentials and only uses images the daemon already holds.

Required (the server refuses to start without them when
`U2S_MCP_BOOTSTRAP=true`):

- `U2S_REDACTO_VERIFY_UBS_MIGRATION_IMAGE`
- `U2S_REDACTO_VERIFY_UBS_CORE_IMAGE`
- `U2S_REDACTO_VERIFY_UBS_RENDERING_IMAGE`

Optional:

- `U2S_REDACTO_VERIFY_UBS_POSTGRES_IMAGE`, default `postgres:16-alpine`
- `U2S_REDACTO_VERIFY_UBS_PLATFORM`, a `docker run --platform` pin
- `U2S_REDACTO_VERIFY_UBS_BOOT_TIMEOUT_SECS`, default 600
- `U2S_REDACTO_VERIFY_UBS_IDLE_TIMEOUT_SECS`, default 1800
- `U2S_REDACTO_VERIFY_UBS_USER` / `_PASSWORD`, the `rendering` service's
  basic auth, default `admin` / `admin`

Run inside the compose stack (`U2S_VERIFY_SELF_CONTAINER` set), the verifier
joins each session's network and reaches `rendering` by name; run on the
host (`cargo run`), it publishes `rendering` on an ephemeral `127.0.0.1`
port instead.

## Tests

`crates/u2s-redacto-verify-core/tests/live_import.rs` and
`crates/u2s-redacto-ubs-verify-mcp/tests/e2e.rs` have `#[ignore]`d tests
that boot platforms, import and render the AAEV fixture, and check that
nothing labelled is left afterwards:

```sh
set -a; . ./.env; set +a
cargo test -p u2s-redacto-verify-core --test live_import -- --ignored
cargo build -p u2s-redacto-ubs-verify-mcp
cargo test -p u2s-redacto-ubs-verify-mcp --test e2e -- --ignored
```

# Running the verifiers on Podman

The AEM and Redacto verifiers run in containers. Docker is the default; Podman
works as well, through its Docker-compatible API. Nothing else changes: the
same images, the same data volume, the same settings.

## Requirements

- Podman **5.3 or newer**, on the client and in the Podman machine. The AEM
  verifier starts its container with `--add-host host.docker.internal:host-gateway`,
  which older Podman refuses. `podman version` shows both.
- On macOS, a Podman machine that is started (`podman machine start`, or Podman
  Desktop). Apple Silicon: the default `applehv`/`libkrun` machine is fine.
  Give it enough memory for AEM (8 GB or more: `podman machine set --memory 8192`).

## Switching to Podman

- **App:** Settings → Verification → Verifier tooling → Container engine →
  Podman, then **restart the app**. The engine is chosen once, at start.
- **CLI:** `--container-engine podman` on any command (for example
  `blueprint verify check --container-engine podman`), or nothing to follow the
  app's setting.

At start the engine finds Podman's API socket (`podman machine inspect` on
macOS, the rootless or system socket on Linux) and points `DOCKER_HOST` at it.
A `DOCKER_HOST` you have set yourself is kept. The readiness check (the app's
banner, `blueprint verify check`) reports a missing socket or a too-old Podman,
and words its hints for Podman: a missing private image names the `podman
login` that takes the `az acr login --expose-token` token, and `podman pull`.
A registry login is read from Podman's `auth.json` as well as Docker's
`config.json`.

## Images and the data volume

The commands in `docker/aem/README.md` and `docker/redacto/README.md` work with
`podman` in place of `docker` (`podman login`, `podman pull`, `podman volume ls`).

`docker/aem/bake-ubs-platform.sh` calls `docker` by name. Run it through a shim
so it reaches Podman, without changing the script:

```sh
mkdir -p ~/.local/podman-shim
ln -sf "$(command -v podman)" ~/.local/podman-shim/docker
PATH="$HOME/.local/podman-shim:$PATH" ./docker/aem/bake-ubs-platform.sh
```

(Homebrew's `podman-docker`-style shims, or Podman Desktop's "Docker
compatibility" option, do the same thing system-wide.)

Images and volumes are per engine: an image pulled into Docker is not in Podman
and the other way round, so pull the images and bake the volume in the engine
you run with.

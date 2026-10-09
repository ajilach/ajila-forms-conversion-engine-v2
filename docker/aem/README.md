# The AEM images the verifiers boot

The AEM verifiers start their own AEM containers from two private,
prebuilt images, each published for arm64 and amd64:

| Image | Verifies | Contents |
|---|---|---|
| `ghcr.io/ajilach/u2s-aem-generic:<tag>` | the generic `aem` format | ajila's AEM Forms base image, as is |
| `ghcr.io/ajilach/u2s-aem-ubs:<tag>` | `aem-ubs` | the base image plus the UBS platform (`ajila-forms-ubs`), the Redacto summary renderer and the OSGi settings they need |

Both are private packages of the `ajilach` GitHub organization. `.env` names
them (`U2S_AEM_VERIFY_UBS_IMAGE`, `U2S_AEM_VERIFY_GENERIC_IMAGE`) and a GitHub
login that can read them (`U2S_AEM_VERIFY_REGISTRY_USERNAME`, and a token with
`read:packages` as `U2S_AEM_VERIFY_REGISTRY_PASSWORD`). Each verifier pulls
its image with that login on its first run, when the Docker daemon does not
have it yet; `verify_status` reports both `aem_image_present` and
`registry_login_configured`. Nothing else is needed to use them.

**Why a seeding image, not a committed one.** The base images declare
`/aem/crx-quickstart` (the JCR repository and quickstart install) a
`VOLUME`, and `docker commit` never captures a volume's contents: a
committed image comes up with an empty repository and crashes (confirmed
live). The verifier therefore always mounts a named volume there, named
after the image (`crates/u2s-aem-verify-core/src/profile.rs`,
`default_data_volume`), and:

- the generic image needs nothing: Docker copies the base image's vanilla
  install into a fresh volume, and AEM's first boot initialises it;
- the UBS image carries the baked volume as `/opt/u2s/crx-quickstart.tar`,
  and its entrypoint ([seed/seed-entrypoint.sh](seed/seed-entrypoint.sh))
  unpacks it into the volume once per image tag before starting AEM.

Both images run AEM on port 8080 (`U2S_AEM_VERIFY_*_CONTAINER_PORT=8080`
in `.env.example`); the verifier's own default of 4502 is a vanilla Adobe
quickstart's.

## Publishing new images

Needed whenever `ajila-forms-ubs` or the base image changes. On a machine
with Docker buildx:

- **Azure CLI**, logged in to ajila's registry (`az login`, then
  `az acr login --subscription BC_AZ_Ajila_10128 --name ajila`), to pull both
  base images (arm64: `ajila.azurecr.io/aemforms-arm:<version>`, amd64:
  `ajila.azurecr.io/aemforms:<version>`).
- **Push access to GitHub's registry**: a GitHub account that can create
  packages in `ajilach`, with `gh auth refresh -s write:packages,read:packages`
  and then `gh auth token | docker login ghcr.io -u <user> --password-stdin`.
- **`ajila-forms-ubs`**, checked out on the branch to verify against (e.g.
  `local-setup/redacto-summary`), with Maven and JDK 8. If the Maven deploy
  fails on a missing dependency, build `ajila-forms-ubs-OP2-mock` first
  (`mvn clean install`) and retry.
- **`ajila-forms-ubs-redacto-summary`**, checked out. A UBS form's submit
  path calls a Redacto renderer (`POST /bin/redacto/summary/generatepdf`);
  the bake installs this bundle into the AEM instance itself, so AEM calls
  itself on localhost. It is written for standalone Sling but needs nothing
  beyond AEM 6.5's APIs.

Then:

```sh
./docker/aem/publish-images.sh <tag> <arm64-base> <amd64-base> \
    <ajila-forms-ubs-dir> <redacto-summary-dir> \
    ./crates/u2s-aem-ubs-verify-mcp/tests/fixtures/global_fragments_20_August.zip
```

It tags the two base images together as the generic image, runs
[bake-ubs-platform.sh](bake-ubs-platform.sh) once per architecture (the
amd64 bake runs under emulation on an Apple silicon machine, slowly),
exports each baked volume into a seeding image, and pushes the UBS image
for both architectures. It refuses to start when any tag it would write
already exists, so pick a new tag (the date) for every publish. Put the
printed tags into `.env.example`.

A new package is private. To let others pull it, give them (or a team) read
access in the package's settings on GitHub (organization `ajilach`,
Packages).

The bake itself Maven-deploys the UBS platform, uploads the fragment
library and the Redacto summary bundle, writes four OSGi configs as
`sling:OsgiConfig` nodes (the Web Console's configMgr endpoint silently
ignores a scripted POST), and rebuilds client libraries. One of the configs
exempts `/bin/redacto` from AEM's authentication requirement, because the
UBS platform posts to the renderer without credentials; this is a local,
disposable verification instance, never a real one.

## Opening a UBS form needs its own mandator/language

A UBS Adaptive Form cannot simply be opened at its `.html` URL the way a
generic AEM form can. Its server-side prefill/DoR metadata service
(`FormMetadataService`) reads `mandator` and `afAcceptLang` from the
request and looks up an entity the form's own authored metadata component
declares -- an unresolvable mandator (empty, or one the form does not
declare) throws `FormMetadataException: No metadata information for
mandator . Formcode: <code>` server-side, a 500 with no client-visible
detail. `u2s-aem-ubs-verify-mcp` derives the right value itself, offline,
from the package's own metadata component before ever opening a browser
(`crates/u2s-aem-ubs-verify-mcp/src/ubs_metadata.rs`) -- nothing to
configure here beyond the optional `U2S_AEM_VERIFY_UBS_MANDATOR` override
(see `.env.example`), for a form that declares more than one entity and
needs a specific one.

Submitting is likewise not a plain `guideBridge.submit()`: on the
`local-setup/redacto-summary` branch this volume is baked from,
`window.forms.ubs.isFWB()` is hardcoded `true`, which hides the wizard
toolbar's own `submit` button on the summary panel entirely (the Forms
WorkBench host is normally what triggers submit instead). A raw
`guideBridge.submit()` would in any case take AEM's native XDP rendering
path, whose 32-bit x86 native services (`XMLForm.exe`, `convertpdf.exe`
under `crx-quickstart/bedrock/svcnative/`) cannot run on this ARM image
(`qemu-i386: Could not open '/lib/ld-linux.so.2'`, confirmed live).
`u2s-aem-ubs-verify-mcp` calls UBS's own
`window.forms.ubs.navigation.submit(...)` routine instead, which populates
the form's `summaryComponent` field before submitting -- the condition
`ajila-forms-ubs`'s own `DorRenderingExecutor.isSummaryOutput()` checks
before routing the submission through Redacto rather than the native path.

## Debugging checklist

If `verify_run` reports `guide_bridge_not_detected` with a near-empty
screenshot, and AEM's `error.log` says `No renderer for extension html` for
a resource type under `/apps/ajila-forms-customers/ajila-forms-ubs`: the
image has no UBS platform in it. The bake checks for that page component
after its Maven deploy and stops when it is missing, so this only happens
with an image baked before that check (`u2s-aem-ubs:2026-10-09-arm64` is
one). A bake once deployed to another AEM on this host: it used a fixed
`localhost:4502`, which Java resolved to a Parallels VM forwarding that
port. The bake now publishes AEM on a random port of `127.0.0.1` only.

If `verify_run` reports `submit_failed`: `window.forms.ubs.navigation` is
missing on the page -- check the UBS clientlib actually loaded (right
branch/package deployed onto the volume).

If it reports `no_download`: pull the AEM container's own
`crx-quickstart/logs/ubsbundle.log` for the submit window.
`FormMetadataException ... mandator` means the mandator never reached the
form data (a URL-parameter problem, not this crate's own logic -- check
`verify_package_check`'s `ubs.selected_mandator`). No `"SummaryOutput:
true"` line from `DorRenderingExecutor` means the `redactoSummary`
DAM flag or the `summaryComponent` field was not actually populated.
`ConvertPdf`/`XMLForm.exe` errors mean the native path was taken after
all. A `404` from `/bin/redacto/summary/generatepdf` means the Redacto
summary bundle is not active in this AEM instance: the image was published
from a bake that did not install it.

If it reports `download_blank`: the submit produced a PDF, but one that
draws nothing on any page (the verifier judges every PDF it gets back with
`u2s_verify_core::pdf_content::blank_pdf`, so a blank is a finding rather
than something a person notices in the review). The Redacto summary bundle
answers `200` with an empty document whenever building the summary HTML
throws: look for `SummaryService Failed to create summary html` in the
container's `error.log`. A `NullPointerException` in
`SummarySchemaParser.buildRowComponent` was a `data-component-type` the
bundle's `ComponentType` did not know (`telephone`, which the UBS summary
widget emits for `guideTelephone`, fixed in the bundle 2026-10-09 together
with a text-box fallback for any later unknown type); rebuild the bundle and
re-run the bake, or install the jar alone the way the bake does. The live
tests (`cargo test -p u2s-aem-ubs-verify-mcp --test e2e -- --ignored`) write
the PDF to `U2S_AEM_VERIFY_LIVE_PDF_OUT` when set, which is what to diagnose
a blank from.

If a live test fails with `aem_not_ready` although AEM is up: the default
container port is 4502, and ajila's images listen on 8080. The tests only
forward `U2S_AEM_VERIFY_CONTAINER_PORT` from the environment, so set it to
`8080` like the other `U2S_AEM_VERIFY_*` values.

A `no_download` whose log shows Redacto *did* render -- a
`rendering summary document at ...` line and
`PDF Document generation and attachment fetching completed`, followed by
`Exception while submitting the form` and
`Invalid name or path: //null` (URL `crx://null`) -- is the OP2 mock, not
this crate. `ProcessPdfaClpResponse`/`ProcessPatchClpResponse` in
`ajila-forms-ubs-OP2-mock` hardcode `isException()` to `false` while
returning a null `uuid` and `pdfDocument`, so
`FormDataPersistenceService` takes its success branch and builds an AEM
Forms `Document` from a null JCR path. The submit then 500s *after* the
PDF already exists, so the download redirect is never added. Those two
responses have to report an exception when they carry no payload; the UBS
bundle then takes the form's authored `formrange_clpmandatory` fallback,
keeps the Redacto PDF and finishes. Note that
`ajila-forms-ubs/bundle/pom.xml` embeds the mock
(`<Embed-Dependency>*;scope=compile|runtime;</Embed-Dependency>`), so
patching the mock alone changes nothing -- the forms bundle has to be
rebuilt and redeployed too. Both Maven builds need JDK 8
(`JAVA_HOME=$(/usr/libexec/java_home -v 1.8)`); on a newer default JDK
their Lombok cannot run and the build fails on unrelated generated
symbols. `aaov-output/README.md` records the full diagnosis.

# Setting up the AEM state `u2s-aem-ubs-verify-mcp` needs

ajila has its own private Docker registry of ready-to-run AEM Forms
images -- there is no vanilla-jar-plus-service-packs build here, unlike a
generic AEM verifier setup. What still needs to happen on top is deploying
the UBS-specific platform (`ajila-forms-ubs`, Maven-deployed) and pointing
its Redacto rendering integration (an OSGi config) at an already-running
rendering dependency.

**That deployed state lives in a Docker volume, never a committed image.**
`ajila.azurecr.io/aemforms-arm` declares `/aem/crx-quickstart` -- the
entire JCR repository and quickstart install -- as a `VOLUME` in its own
Dockerfile. `docker commit` only ever captures a container's own writable
layer, never a volume's contents, so a container booted from a *committed*
derivative of this image comes up with an empty `/aem/crx-quickstart`: no
quickstart jar, no `conf/sling.properties`, nothing -- confirmed live, the
resulting image crashed on boot. There is no flag or workaround for this;
it is how Docker volumes work by design. The fix is to never commit: boot
the *always-vanilla* base image with a named volume explicitly attached at
`/aem/crx-quickstart`, deploy onto that, and keep pointing every future
boot at the same volume (`U2S_AEM_VERIFY_UBS_DATA_VOLUME`) instead of a
baked image tag.

Two stages. The first you run once per branch/service-pack level you want
to verify against; the second just points u2s at the result -- there is no
`just aem-warm` step for this profile (see below for why).

## 1. One-time setup

- **Azure CLI**, authenticated against ajila's registry:
  ```sh
  brew install azure-cli
  az login
  az acr login --subscription BC_AZ_Ajila_10128 --name ajila
  ```
  `az login` needs your own browser-based Azure AD auth -- nothing here can
  do that for you.
- **`ajila-forms-ubs`**, checked out locally on the branch you want to
  verify (e.g. `local-setup/redacto-summary`). This is the UBS-specific
  platform bundle; the bake below Maven-deploys it, it does not build it
  from anything in this repository.
- **`ajila-forms-ubs-redacto-summary`**, checked out locally. A UBS form's
  submit path calls a Redacto renderer at render time
  (`POST /bin/redacto/summary/generatepdf`), and this is the bundle that
  answers it. The bake below installs it **into the AEM instance itself**,
  so there is no second container to run, keep alive or remember to
  redeploy -- AEM simply calls itself on localhost.

  The bundle is written for standalone Apache Sling and ships its own
  `docker-compose.yml` for that, but nothing in it requires standalone
  Sling: it declares only old, conservative `provided` APIs (Sling API
  2.9, OSGi 4.2, servlet 2.5, JCR 2.0 -- all satisfied by AEM 6.5), embeds
  every heavy rendering dependency of its own (openhtmltopdf, PDFBox,
  Batik, jsoup), marks its remaining imports `resolution:=optional`, and
  carries its own fonts, profiles and template as bundle resources. It
  registers by `sling.servlet.paths`, so it answers on AEM's own port with
  no further wiring.

  If you would rather run it as a separate service anyway (its
  `docker-compose.yml` publishes `18080`), pass the checkout to the bake
  script as usual and set `REDACTO_URL` to point the UBS platform at that
  instance instead. Note that container has no volume, so a
  `docker compose down` silently loses the deployed bundle and the next
  boot answers every request `404` with nothing in its logs naming why.

## 2. Populate the data volume

```sh
docker pull --platform linux/arm64 ajila.azurecr.io/aemforms-arm:6.5.17.0

./docker/aem/bake-ubs-platform.sh \
    ajila.azurecr.io/aemforms-arm:6.5.17.0 \
    u2s-aem-ubs-data \
    /path/to/your/ajila-forms-ubs \
    /path/to/your/ajila-forms-ubs-redacto-summary \
    linux/arm64
```

`bake-ubs-platform.sh` creates the named volume (a no-op if it already
exists), boots the base image with it attached at `/aem/crx-quickstart`,
waits for AEM to report ready, runs `mvn clean install -PautoInstallPackage`
from your `ajila-forms-ubs` checkout against it, builds and uploads the
Redacto summary bundle from your `ajila-forms-ubs-redacto-summary`
checkout, sets the three OSGi configs the wiki's "Local Setup Guide
(Docker)" page names (`ConfigurationService`'s Redacto URL,
`OnlineFormsAuthenticationConfiguration`'s exclusion list,
`DownloadService`'s protection flag) plus one more the in-AEM renderer
needs (below), rebuilds client libraries, then stops the container -- the
deployed state stays in the volume. Safe to re-run against the same volume
name later (a new branch, a changed checkout): the package install, the
bundle upload and the OSGi config writes all replace prior state rather
than erroring.

Since the renderer now runs inside AEM, the Redacto URL is
`http://localhost:8080/bin/redacto/summary/generatepdf` -- AEM reaching
its own servlet inside its own container, so neither `localhost:18080` nor
`host.docker.internal`. Override it with a `REDACTO_URL` environment
variable to point at an external renderer instead.

The extra OSGi config is an authentication exemption. `ajila-forms-ubs`'s
own `RedactoIntegrationService` posts to the renderer with no credentials
at all ("no authentication for now", in its source), which was free
against a standalone Sling container with nothing in front of it. Inside
AEM the same request gets a `401`, because AEM's stock
`sling.auth.requirements` is `+/` -- authentication required everywhere.
The script rewrites that property with its four stock entries plus
`-/bin/redacto`. It repeats the defaults because writing the property
replaces the whole list; dropping `+/` would leave the entire instance
unauthenticated. This is a local, disposable verification instance that
already runs with the UBS platform's own authentication, servlet filter
and download protection switched off, so one more local-only exemption is
in keeping with it -- but it has no business anywhere real.

The bundle is uploaded to the Web Console rather than deployed with its
own `autoInstallBundle` profile, because that profile's maven-sling-plugin
configuration hardcodes `<slingUrl>http://localhost:18080/system/console</slingUrl>`
in the pom. An explicit plugin configuration beats `-Dsling.url`, so that
profile would quietly deploy to a standalone Sling container (or fail when
none is running) no matter what target you asked for. The script also
starts the bundle explicitly: `-F start=start` on the install action was
confirmed live to leave it in state `Installed` rather than `Active`.

OSGi config is written as a `sling:OsgiConfig` JCR node under
`/apps/system/config/<pid>` (the same mechanism a content package uses to
ship configuration), not scripted against `/system/console/configMgr/<pid>`
the way a person would through the browser: a curl multipart POST to that
endpoint was tried first and, live against a real 6.5.17.0 instance,
returned 200 OK without ever actually persisting a value -- read back
immediately after, every property still showed the metatype default
(`is_set: false`). This may well be a missing piece of what the Web
Console's own JS sends alongside the form (a CSRF token, most likely) that
a browser supplies automatically and a bare curl script does not
reconstruct -- not evidence the browser UI itself is broken. If you are
setting one of these by hand rather than running the script, either use
the Web Console UI directly (`/system/console/configMgr`) in a real
browser, or CRXDE Lite to create the same `sling:OsgiConfig` node this
script writes.

If the Maven deploy fails on a missing dependency, build
`ajila-forms-ubs-OP2-mock` first (`mvn clean install`, no special profile
-- it only needs to land in your local Maven repository) and retry.

## 3. Point u2s at it

```sh
export U2S_AEM_VERIFY_UBS_IMAGE=ajila.azurecr.io/aemforms-arm:6.5.17.0
export U2S_AEM_VERIFY_UBS_DATA_VOLUME=u2s-aem-ubs-data
export U2S_AEM_VERIFY_UBS_CONTAINER_PORT=8080
export U2S_AEM_VERIFY_UBS_USER=admin
export U2S_AEM_VERIFY_UBS_PASSWORD=admin
# Leave U2S_AEM_VERIFY_UBS_REDACTO_URL unset when the renderer is baked
# into AEM (the default since step 2 installs it there). It configures a
# host-side reachability pre-check for a *separately running* renderer, so
# that `verify_run` refuses up front rather than letting a submit fail
# invisibly inside AEM. An in-AEM renderer needs no such check -- if AEM
# is up, so is it -- and the check could not be written anyway, since the
# verifier publishes the AEM container on a fresh random host port each
# run. Set it only when REDACTO_URL pointed the platform at an external
# renderer, and then to an address reachable *from the host*:
# export U2S_AEM_VERIFY_UBS_REDACTO_URL=http://localhost:18080/bin/redacto/summary/generatepdf
# Optional: only needed when a form's own metadata component declares more
# than one mandator entity and the first one is not the one you want --
# see U2S_AEM_VERIFY_UBS_MANDATOR in .env.example.
# export U2S_AEM_VERIFY_UBS_MANDATOR=033
```

`U2S_AEM_VERIFY_UBS_IMAGE` stays the *vanilla* base image here -- never a
committed derivative, for the reason above. `U2S_AEM_VERIFY_UBS_DATA_VOLUME`
is what actually carries the deployed state: `crate::session::boot` attaches
it at `/aem/crx-quickstart` on every AEM container this profile boots, so
each boot starts from the already-deployed platform rather than a vanilla
install. `U2S_AEM_VERIFY_UBS_CONTAINER_PORT` matters too and is easy to get
wrong: this crate's own default (4502) is a *vanilla Adobe quickstart jar's*
default, but ajila's image runs its entrypoint with an explicit `-p 8080`
(`docker inspect`'s own `ExposedPorts`/`AEM_START_OPTS` confirm it) --
leaving this unset publishes a port nothing inside the container is
listening on, and every `verify_run` times out waiting for a login page
that can never answer (confirmed live).

**No `just aem-warm` for this profile, and none is needed.** That command
exists to skip a slow first-boot bundle-activation cost by committing an
already-booted container -- exactly the operation that cannot work here.
`crate::warm::warm_command` (shared by every AEM-verifying binary, in
`crates/u2s-aem-verify-core`) refuses outright when
`U2S_AEM_VERIFY_UBS_DATA_VOLUME` is set, rather than silently building a
broken image. The volume already gives every boot the fast-start benefit
`aem-warm` would otherwise exist to provide.

From here `verify_run` keeps one AEM instance running across calls rather
than rebooting per call (`crates/u2s-aem-verify-core/src/session.rs`) --
`U2S_AEM_VERIFY_UBS_REDACTO_URL` must stay reachable for as long as that
session is up, and `verify_status` reports whether it currently is. The
same session also backs `verify_open` and its sibling interactive control
tools (`verify_controls`/`verify_set`/`verify_next`/`verify_prev`/
`verify_reset`/`verify_screenshot`/`verify_submit`/`verify_close`,
`crates/u2s-aem-verify-core/src/interactive.rs`), for driving a form one
step at a time instead of walking it in one shot -- `verify_status`'s own
`session_open_form` names a form left open by a run that never called
`verify_close`.

Re-run stage 2 whenever the ACR base image tag or your `ajila-forms-ubs`
branch changes -- against the same volume name, so the change lands in the
state every future boot already reuses.

## 4. Opening a UBS form needs its own mandator/language

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
above, for a form that declares more than one entity and needs a specific
one.

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
all. If the Redacto summary bundle itself was redeployed recently, re-run
its smoke test above -- a `docker compose down` on that service silently
drops the deployed bundle (see step 1).

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

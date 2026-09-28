#!/usr/bin/env sh
# Populates a named Docker volume with a fully-deployed UBS platform, for
# U2S_AEM_VERIFY_UBS_DATA_VOLUME to point at: pulls ajila's own AEM Forms
# image, boots it with the volume attached at /aem/crx-quickstart,
# Maven-deploys the UBS forms platform (ajila-forms-ubs) onto it, sets the
# three OSGi settings a submitted form's Redacto rendering path needs, then
# stops the container. See README.md for the one-time setup (az login, a
# checked-out ajila-forms-ubs, a reachable Redacto rendering dependency)
# this assumes is already done.
#
# No image is committed. ajila.azurecr.io/aemforms-arm declares
# /aem/crx-quickstart as a VOLUME in its own Dockerfile, and `docker commit`
# never captures a volume's contents -- only a container's own writable
# layer -- so a committed image here would silently be missing the entire
# JCR repository. Confirmed live: an earlier version of this script did
# exactly that, and the resulting image crashed on boot, missing its own
# quickstart jar and crx-quickstart/conf/sling.properties. The volume
# itself is the durable artifact instead -- attach it (via
# U2S_AEM_VERIFY_UBS_DATA_VOLUME) to any container booted from the
# always-vanilla base image and the deployed state is already there. No
# `just aem-warm` step either: that command refuses to run against a
# data-volume profile (see `u2s-aem-verify-mcp warm_command`'s own doc) for
# the same reason -- the volume already makes every boot fast.
#
# Usage:
#   ./bake-ubs-platform.sh <base-image> <volume-name> <ajila-forms-ubs-dir> \
#       <redacto-summary-dir> <fragments-package.zip> [platform]
#
# Example:
#   ./bake-ubs-platform.sh ajila.azurecr.io/aemforms-arm:6.5.17.0 \
#       u2s-aem-ubs-data \
#       ~/Documents/ajila-forms-ubs \
#       ~/Documents/ajila-forms-ubs-redacto-summary \
#       ./crates/u2s-aem-ubs-verify-mcp/tests/fixtures/global_fragments_20_August.zip \
#       linux/arm64
#
# The Redacto summary renderer is installed *into this AEM instance*, not
# run as a separate Apache Sling container alongside it. The bundle is
# built for standalone Sling, but nothing in it needs to be: it declares
# only old, conservative `provided` APIs (Sling API 2.9, OSGi 4.2, servlet
# 2.5, JCR 2.0 -- all satisfied by 6.5), embeds every heavy rendering
# dependency of its own (openhtmltopdf, PDFBox, Batik, jsoup) via bnd
# `-includeresource`, marks all remaining imports `resolution:=optional`,
# and ships its own fonts/profiles/template as bundle resources. It
# registers its servlet by `sling.servlet.paths`, so it answers on AEM's
# own port with no extra wiring. One container instead of two, and no
# `host.docker.internal` hop: the UBS platform's Redacto URL becomes
# AEM calling itself on localhost.
#
# Set REDACTO_URL to point the UBS platform at an external renderer
# instead; the bundle is still installed either way, it just goes unused.
#
# Safe to re-run against the same volume name: a UBS platform redeploy is
# `force=true` (overwrites), a content-package upload is `force=true` too,
# a bundle re-upload replaces the prior revision, and each OSGi config
# write replaces the whole node, so re-running after a new checkout
# converges to the new state rather than erroring.

set -eu

if [ "$#" -lt 5 ]; then
    echo "usage: $0 <base-image> <volume-name> <ajila-forms-ubs-dir> <redacto-summary-dir> <fragments-package.zip> [platform]" >&2
    exit 1
fi

BASE_IMAGE="$1"
VOLUME_NAME="$2"
UBS_DIR="$3"
REDACTO_DIR="$4"
FRAGMENTS_PACKAGE="$5"
PLATFORM="${6:-linux/amd64}"

# Argument 5 is new, and used to be where the platform went. A platform
# string is unmistakable, so say so rather than trying to upload
# "linux/arm64" to the Package Manager.
case "$FRAGMENTS_PACKAGE" in
    linux/*|darwin/*|windows/*)
        echo "error: argument 5 is now the Adaptive Forms fragment-library content" >&2
        echo "       package (a .zip), and the platform has moved to argument 6." >&2
        echo "       UBS forms reference shared fragments by absolute repository" >&2
        echo "       path; without them a form still renders, but every fragment" >&2
        echo "       reference in it silently resolves to nothing." >&2
        exit 1
        ;;
esac

if [ ! -f "$FRAGMENTS_PACKAGE" ]; then
    echo "error: no such fragment-library package: $FRAGMENTS_PACKAGE" >&2
    exit 1
fi

# Argument 4 used to be the Redacto URL, back when the renderer was a
# separate long-lived Sling container this script only pointed at. Fail
# loudly rather than treating a URL as a directory and failing later with
# something unrecognisable.
case "$REDACTO_DIR" in
    http://*|https://*)
        echo "error: argument 4 is now the ajila-forms-ubs-redacto-summary checkout" >&2
        echo "       directory, not a URL -- the renderer is installed into this AEM" >&2
        echo "       instance rather than run beside it. To point the UBS platform at" >&2
        echo "       an external renderer anyway, pass the checkout here and set" >&2
        echo "       REDACTO_URL=$REDACTO_DIR in the environment." >&2
        exit 1
        ;;
esac

# AEM reaching its own servlet, inside its own container -- not the host,
# so neither localhost:18080 nor host.docker.internal.
REDACTO_URL="${REDACTO_URL:-http://localhost:8080/bin/redacto/summary/generatepdf}"

AEM_USER="${AEM_USER:-admin}"
AEM_PASSWORD="${AEM_PASSWORD:-admin}"
CONTAINER_NAME="aem-bake-ubs-$$"

# Two AEM instances cannot share one repository. Oak takes a lock on the
# segment store and the loser does not fail -- it blocks, indefinitely and
# silently, at "Creating file store", with an OSGi console that answers
# and every content path returning 404. The readiness poll below then spins
# forever against a login page that will never exist. Confirmed live,
# against a leftover `u2s-verify-aem-*` container: the verifier keeps its
# AEM session alive for reuse after a run, so one can still hold this
# volume minutes later. Refuse up front instead.
HOLDERS="$(docker ps --filter "volume=$VOLUME_NAME" --format '{{.Names}}' || true)"
if [ -n "$HOLDERS" ]; then
    echo "error: these running containers already have $VOLUME_NAME mounted:" >&2
    echo "$HOLDERS" | sed 's/^/         /' >&2
    echo "       Two AEM instances on one repository do not fail cleanly -- the second" >&2
    echo "       blocks forever opening the segment store. Stop them first:" >&2
    echo "         docker stop $(echo "$HOLDERS" | tr '\n' ' ')" >&2
    exit 1
fi

echo "creating volume $VOLUME_NAME (a no-op if it already exists) ..."
docker volume create "$VOLUME_NAME" >/dev/null

echo "starting $BASE_IMAGE as $CONTAINER_NAME with $VOLUME_NAME attached at /aem/crx-quickstart ..."
docker run -d --name "$CONTAINER_NAME" --platform "$PLATFORM" \
    --add-host=host.docker.internal:host-gateway \
    -p 4502:8080 \
    -v "$VOLUME_NAME:/aem/crx-quickstart" \
    "$BASE_IMAGE" >/dev/null

cleanup() {
    docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# A single 200 here is not enough: live testing found AEM can answer the
# login page once, then 503 again for a stretch (a repository/HTTP-service
# restart as later-starting bundles settle) before it is actually done --
# and package installation started during that window fails outright
# (content-package-maven-plugin sees a non-package response from
# /crx/packmgr). Three consecutive successes is what live testing needed to
# reliably clear that window.
echo "waiting for AEM to report ready (first boot into an empty volume can take several minutes; a re-run against an already-populated volume is much faster) ..."
STABLE=0
while [ "$STABLE" -lt 3 ]; do
    if curl -fsS -o /dev/null --max-time 5 "http://localhost:4502/libs/granite/core/content/login.html"; then
        STABLE=$((STABLE + 1))
    else
        STABLE=0
    fi
    sleep 10
done
echo "ready."

echo "deploying the UBS forms platform from $UBS_DIR ..."
( cd "$UBS_DIR" && JAVA_HOME="$(/usr/libexec/java_home -v 1.8)" mvn -q clean install -PautoInstallPackage )

# Built without the project's own `autoInstallBundle` profile on purpose:
# that profile's maven-sling-plugin config hardcodes
# `<slingUrl>http://localhost:18080/system/console</slingUrl>` in the pom,
# which beats `-Dsling.url` and would silently deploy to a standalone Sling
# container (or fail if none is up) instead of to AEM. Uploading the built
# jar to the Web Console directly is what that profile does anyway, and it
# takes the target from this script rather than from the other repository.
# The fragment libraries UBS forms reference by absolute repository path
# (/content/dam/formsanddocuments/afforms_{global,ubs}_fragmentlib and their
# /content/forms/af counterparts). A form referencing a fragment that is not
# in the repository does not fail: it renders with that fragment resolving
# to nothing, so the form comes up looking structurally fine while whole
# panels are quietly empty, and a rendered Document of Record comes out
# near-blank. AAOV_033 alone pulls in seven of them. Installed through the
# CRX Package Manager, the same way a person would upload it in the browser.
#
# `force=true` on the upload replaces an already-uploaded copy rather than
# erroring, which is what makes re-running this script converge. The
# install target is read back from the upload's own response rather than
# assembled from the group/name in the package's properties.xml, because
# the uploaded path follows the *filename*, which carries a date here.
echo "uploading $(basename "$FRAGMENTS_PACKAGE") to the CRX Package Manager ..."
UPLOAD_RESPONSE="$(curl -fsS -u "$AEM_USER:$AEM_PASSWORD" \
    -F "package=@$FRAGMENTS_PACKAGE" \
    "http://localhost:4502/crx/packmgr/service/.json/?cmd=upload&force=true")"

FRAGMENTS_PATH="$(printf '%s' "$UPLOAD_RESPONSE" | python3 -c '
import json, sys
response = json.load(sys.stdin)
if not response.get("success"):
    sys.exit("package upload failed: " + str(response.get("msg")))
print(response["path"])
')"

echo "installing $FRAGMENTS_PATH ..."
curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -X POST \
    "http://localhost:4502/crx/packmgr/service/.json$FRAGMENTS_PATH?cmd=install" \
    | python3 -c '
import json, sys
response = json.load(sys.stdin)
if not response.get("success"):
    sys.exit("package install failed: " + str(response.get("msg")))
print("  " + str(response.get("msg")))
'

# A 200 from the install command only means the request was accepted, so
# check the content is actually readable at the path forms resolve it by.
echo "verifying the fragment libraries resolve ..."
for fragment_root in \
    /content/dam/formsanddocuments/afforms_global_fragmentlib \
    /content/dam/formsanddocuments/afforms_ubs_fragmentlib \
    /content/forms/af/afforms_global_fragmentlib \
    /content/forms/af/afforms_ubs_fragmentlib
do
    if ! curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -o /dev/null \
            "http://localhost:4502$fragment_root.json"; then
        echo "error: $fragment_root is not readable after installing the package;" >&2
        echo "       forms referencing it would render it as nothing." >&2
        exit 1
    fi
    echo "  $fragment_root"
done

# `ajila-forms-ubs`'s summary control calls `DOMPurify.sanitize(..)` without
# shipping DOMPurify or declaring a dependency on anything that does -- on a
# real UBS page the Forms WorkBench host has already put it on `window`.
# Without it `setSummaryData` throws on its first call, and does so
# invisibly: the wizard still advances, the form still submits, a PDF still
# comes back -- but the summary component's value is never set, and that
# value is the whole of what Redacto renders, so the PDF holds only the
# form's header and title. See docker/aem/dompurify/README.md.
DOMPURIFY_DIR="$(dirname "$0")/dompurify"
DOMPURIFY_CATEGORY="u2s.verify.dompurify"
DOMPURIFY_ROOT="/apps/u2s-verify-support/clientlibs/dompurify"
# The clientlib that actually loads on a form page, which embeds the summary
# one; hanging the dependency here is what gets DOMPurify emitted ahead of
# `summary.js` rather than merely present in the repository.
SUMMARY_CLIENTLIB="/apps/ajila-forms-customers/ajila-forms-ubs/clientlibs/components/ajila-forms-ubs-summary/common"

echo "installing DOMPurify as $DOMPURIFY_CATEGORY ..."
curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -o /dev/null -X POST \
    "http://localhost:4502$DOMPURIFY_ROOT" \
    -F "jcr:primaryType=cq:ClientLibraryFolder" \
    -F "categories=$DOMPURIFY_CATEGORY" \
    -F "categories@TypeHint=String[]"
# No trailing slash on the path: with one, Sling's POST servlet takes it as
# "create a child under here with a generated name", invents something like
# `1_1790227457144`, and drops the file parts on the floor -- a 200, an empty
# client library, and no hint that anything went wrong. Without it, each part
# name becomes the child node's name, which is what an `nt:file` needs.
curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -o /dev/null -X POST \
    "http://localhost:4502$DOMPURIFY_ROOT" \
    -F "purify.min.js=@$DOMPURIFY_DIR/purify.min.js" \
    -F "js.txt=@$DOMPURIFY_DIR/js.txt"

# A 200 from the POST does not mean the library is servable, and an empty
# client library is exactly the silent failure this whole exercise was about.
if ! curl -fsS -u "$AEM_USER:$AEM_PASSWORD" \
        "http://localhost:4502$DOMPURIFY_ROOT.js" | grep -q 'DOMPurify'; then
    echo "error: $DOMPURIFY_ROOT.js does not serve DOMPurify after installing it;" >&2
    echo "       the summary component would fail with 'DOMPurify is not defined'" >&2
    echo "       and every rendered PDF would come back holding only a header." >&2
    exit 1
fi

echo "making the summary client library depend on it ..."
curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -o /dev/null -X POST \
    "http://localhost:4502$SUMMARY_CLIENTLIB" \
    -F "dependencies=$DOMPURIFY_CATEGORY" \
    -F "dependencies@TypeHint=String[]"

echo "building the Redacto summary renderer from $REDACTO_DIR ..."
( cd "$REDACTO_DIR" && JAVA_HOME="$(/usr/libexec/java_home -v 1.8)" mvn -q clean install -DskipTests )

REDACTO_JAR="$(ls "$REDACTO_DIR"/target/ajila-forms-ubs-redacto-summary-bundle-*.jar 2>/dev/null | head -1)"
if [ -z "$REDACTO_JAR" ]; then
    echo "error: no built bundle jar under $REDACTO_DIR/target" >&2
    exit 1
fi

# `-F start=start` on the install action does not reliably leave the bundle
# started (confirmed live: it lands in state Installed, not Active), so the
# start is a separate, explicit call rather than a flag trusted to take.
echo "installing $(basename "$REDACTO_JAR") into AEM ..."
curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -o /dev/null \
    -F action=install -F bundlestartlevel=20 \
    -F "bundlefile=@$REDACTO_JAR" \
    "http://localhost:4502/system/console/bundles"

echo "starting the Redacto summary bundle ..."
until curl -fsS -u "$AEM_USER:$AEM_PASSWORD" \
        "http://localhost:4502/system/console/bundles/com.ajila.redacto.summary.bundle.json" \
        | grep -q '"state":"Active"'; do
    curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -o /dev/null -X POST -F action=start \
        "http://localhost:4502/system/console/bundles/com.ajila.redacto.summary.bundle" || true
    sleep 2
done

# Writes a `sling:OsgiConfig` JCR node under /apps/system/config, named
# after the PID -- Sling's own JCR installer watches that path and turns
# the node straight into a live OSGi Configuration. This is *not* the same
# as posting to /system/console/configMgr/<pid>: that endpoint's own
# multipart save form was tried first and confirmed, live, to accept the
# POST (200 OK) without actually persisting anything -- every property
# still read back as the metatype default (`is_set: false`) no matter what
# was posted, with or without `propertylist`, `action=ajax`, or a Referer
# header. The JCR-node route is also how a content package ships OSGi
# config in the first place, so this script now installs config the same
# way `ajila-forms-ubs` itself would.
set_osgi_config() {
    pid="$1"
    shift
    curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -X POST "http://localhost:4502/apps/system/config/$pid" \
        -F "jcr:primaryType=sling:OsgiConfig" "$@" >/dev/null
}

echo "pointing the UBS platform's Redacto rendering integration at $REDACTO_URL ..."
set_osgi_config "com.ubs.OP2.forms.internal.service.ConfigurationService" \
    -F "com.ubs.OP2.forms.redacto.url=$REDACTO_URL" \
    -F 'com.ubs.OP2.forms.redacto.summary=true' \
    -F 'com.ubs.OP2.forms.redacto.summary@TypeHint=Boolean'

echo "excluding the Redacto callback path from the online forms auth filter ..."
set_osgi_config "com.ubs.OP2.forms.internal.web.security.OnlineFormsAuthenticationConfiguration" \
    -F 'authenticationEnabled=false' -F 'authenticationEnabled@TypeHint=Boolean' \
    -F 'servletFilterEnabled=false' -F 'servletFilterEnabled@TypeHint=Boolean' \
    -F 'urlExclusionList=/bin/com/ajila' -F 'urlExclusionList@TypeHint=String[]'

echo "disabling the download-protection filter ..."
set_osgi_config "com.ubs.OP2.forms.internal.service.filedownload.DownloadService" \
    -F 'protectionEnabled=false' -F 'protectionEnabled@TypeHint=Boolean'

# `ajila-forms-ubs`'s own RedactoIntegrationService posts to the renderer
# with no credentials at all ("no authentication for now", in its source).
# That was free when the renderer was a standalone Sling container with no
# authentication in front of it; inside AEM the same request gets a 401,
# because AEM's stock `sling.auth.requirements` is `+/` -- authentication
# required everywhere. Exempting just this one path restores the previous
# behaviour rather than widening anything else. The four defaults are
# repeated because writing this property replaces the whole list; dropping
# `+/` would leave the entire instance unauthenticated.
#
# This is a local, disposable verification instance that already runs with
# the UBS platform's own authentication, servlet filter and download
# protection switched off (above), so one more local-only exemption is in
# keeping with it. Do not carry this configuration anywhere real.
echo "exempting the Redacto servlet path from AEM's authentication requirement ..."
set_osgi_config "org.apache.sling.engine.impl.auth.SlingAuthenticator" \
    -F 'sling.auth.requirements=+/' \
    -F 'sling.auth.requirements=-/libs/granite/core/content/login' \
    -F 'sling.auth.requirements=-/etc.clientlibs' \
    -F 'sling.auth.requirements=-/etc/clientlibs/granite' \
    -F 'sling.auth.requirements=-/libs/dam/remoteassets/content/loginerror' \
    -F 'sling.auth.requirements=-/bin/redacto' \
    -F 'sling.auth.requirements@TypeHint=String[]'

# The JCR installer needs a moment to notice each new node and apply it as
# a live OSGi Configuration -- confirmed live to need at most a few seconds,
# but there is no event to wait on here, so this polls the same way
# `verify_run`'s own readiness waits do rather than guessing a fixed sleep.
echo "waiting for the OSGi installer to apply the new configuration ..."
until curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -X POST \
        "http://localhost:4502/system/console/configMgr/com.ubs.OP2.forms.internal.service.ConfigurationService" \
        | grep -q '"is_set":true'; do
    sleep 2
done

echo "rebuilding client libraries ..."
curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -X POST "http://localhost:4502/libs/granite/ui/content/dumplibs.rebuild.html" \
    -F cmd=invalidateCaches >/dev/null
curl -fsS -u "$AEM_USER:$AEM_PASSWORD" -X POST "http://localhost:4502/libs/granite/ui/content/dumplibs.rebuild.html" \
    -F cmd=rebuildAll >/dev/null

echo "stopping $CONTAINER_NAME (the deployed state stays in $VOLUME_NAME) ..."
docker stop -t 300 "$CONTAINER_NAME" >/dev/null

echo "done: $VOLUME_NAME now holds a deployed UBS platform and its Redacto renderer."
echo "export:"
echo "  U2S_AEM_VERIFY_UBS_IMAGE=$BASE_IMAGE            # the vanilla base image -- never a committed derivative"
echo "  U2S_AEM_VERIFY_UBS_DATA_VOLUME=$VOLUME_NAME"
echo "the UBS platform renders via $REDACTO_URL."
case "$REDACTO_URL" in
    http://localhost:8080/*)
        echo "leave U2S_AEM_VERIFY_UBS_REDACTO_URL unset: that variable pre-checks a"
        echo "separately running renderer from the host, and this one is inside AEM."
        ;;
    *)
        echo "  U2S_AEM_VERIFY_UBS_REDACTO_URL=<that renderer, as reachable from the host>"
        ;;
esac
echo "then register/restart u2s-aem-ubs-verify-mcp. No warm step is needed or possible for this profile."

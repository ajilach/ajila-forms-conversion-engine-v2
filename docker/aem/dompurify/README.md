# Vendored DOMPurify

`purify.min.js` is DOMPurify 3.2.7, unmodified, dual-licensed MPL-2.0 or
Apache-2.0 (`LICENSE` here; the attribution header is also inside the
minified file). Upstream: https://github.com/cure53/DOMPurify

## Why this is in this repository

`ajila-forms-ubs`'s summary control calls `DOMPurify.sanitize(..)`
(`clientlibs/components/ajila-forms-ubs-summary/integration/js/summary.js`)
but neither ships the library nor declares a `dependencies` on anything
that does -- its clientlib folder sets only `categories` and `embed`. On a
real UBS page the surrounding Forms WorkBench host provides the global,
which is consistent with that branch hardcoding `isFWB()` to `true`. A bare
AEM instance does not, so `setSummaryData` throws
`ReferenceError: DOMPurify is not defined` on its first call.

That failure is silent in every way that matters: the panel still advances,
the form still submits, a PDF still comes back. But the summary component's
value is never set, and that value is the entirety of what Redacto renders
-- so the PDF contains the form's header and title and nothing else.
Diagnosed live; every other link in the chain (the Redacto bundle, the
summary component, its clientlib, `resolveNode("summaryComponent")`) was
already healthy.

`bake-ubs-platform.sh` installs this as its own client library and makes
the summary's own clientlib depend on it, so the global exists before
`summary.js` runs.

## Why vendored rather than fetched

AGENTS.md's self-containment rule: the project references only files inside
it. A bake that curls a CDN would also make a reproducible local
environment depend on the network and on whatever that URL serves that day.

## The real fix lives elsewhere

This compensates for a packaging gap in `ajila-forms-ubs`: a clientlib that
uses a library should declare or embed it, rather than relying on a host
page to have put it on `window` first. Fixing it there would make this
directory unnecessary.

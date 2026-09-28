# Schema

`app_redacto_baseline.sql` is a verbatim copy of
`ajila-redacto-migration/sql/V1__baseline_schema.sql` from
`~/Documents/ajila-redacto-platform` (branch `develop`), the platform's own
Flyway baseline for the `app_redacto` schema -- copied in per this
workspace's self-containment rule (AGENTS.md: "the project should only
reference files within the project") rather than read from that repo at
runtime.

[`crate::session`] applies it verbatim to a throwaway container's own
`app_redacto` schema before importing a dump under test. It never needs to
change unless the platform's own baseline migration does.

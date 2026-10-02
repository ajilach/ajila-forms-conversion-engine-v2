-- Vendored from ajila-redacto-platform
-- (ajila-redacto-core/src/main/resources/database/local/bootstrap.sql), which
-- u2s-redacto-verify-core's session boot runs once per platform it boots
-- (src/session.rs, embedded with include_str!). Keep in step with that file
-- when the platform's role or schema name changes.

-- LOCAL DEVELOPMENT ONLY -- do not run this against a deployed database.
--
-- Creates the things Flyway cannot create for itself because they live outside the
-- schema and need superuser rights: the application role, the schema, and the
-- privileges Flyway needs to write its history table.
--
-- In a deployed environment this step is the platform's responsibility (Azure
-- Flexible Server + Key Vault / managed identity), NOT a migration:
--   * role creation there requires membership in `azure_pg_admin`
--   * the password must come from Key Vault, never from a file in the repo
--   * the schema name is environment-specific (`REDACTO_DB_SCHEMA`)
-- See [[guides/container-deployment]].
--
-- Run as the `postgres` superuser once per platform a verifier session boots.
-- Idempotent.
--
-- Note: the schema is named after the role on purpose. The DAOs use
-- `@Table(name = "...")` with no schema and `hibernate.cfg.xml` sets no
-- `hibernate.default_schema`, so local resolution relies on PostgreSQL's default
-- `search_path = "$user", public` matching schema `app_redacto` to user
-- `app_redacto`. Renaming one without the other breaks local startup.

DO
$$
    BEGIN
        IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'app_redacto') THEN
            CREATE ROLE app_redacto LOGIN PASSWORD 'password';
        END IF;
    END
$$;

CREATE SCHEMA IF NOT EXISTS app_redacto AUTHORIZATION app_redacto;
GRANT USAGE, CREATE ON SCHEMA app_redacto TO app_redacto;

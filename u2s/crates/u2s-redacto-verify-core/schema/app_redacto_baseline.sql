-- Baseline schema for Redacto core.
--
-- Applied by Flyway in every environment (local Compose and Azure PostgreSQL).
--
-- Conventions for this and every future migration in this folder:
--
--   * Table names are UNQUALIFIED. Flyway sets the connection search_path to the
--     configured schema (`FLYWAY_SCHEMAS`), so the same file works against
--     `app_redacto` locally and whatever `REDACTO_DB_SCHEMA` names in a deployed
--     environment. This mirrors how the entities resolve at runtime: the DAOs use
--     `@Table(name = "...")` with no schema, and Hibernate relies on the same
--     search_path (or `hibernate.default_schema` when `REDACTO_DB_SCHEMA` is set).
--   * No CREATE ROLE / GRANT / OWNER TO. Roles, schema ownership and the database
--     itself are provisioned outside the migration: `database/local/bootstrap.sql`
--     locally, and the Azure platform (Key Vault / managed identity) in deployed
--     environments. Flyway connects as an already-existing application user.
--   * NEVER edit a migration that has been applied anywhere -- Flyway checksums
--     each file and will fail on drift. Add a new V<n>__ file instead.
--
-- `IF NOT EXISTS` is kept so this baseline is a safe no-op on volumes whose schema
-- predates Flyway (see baselineOnMigrate in docker-compose.yml).

create table if not exists assets
(
    id         varchar(36) not null unique,
    created    timestamp,
    asset_id   varchar(50),
    asset_type varchar(20),
    primary key (id)
);

create table if not exists asset_version
(
    id           varchar(36) not null unique,
    created      timestamp,
    language     varchar(20),
    version      bigint,
    status       varchar(20),
    content      text,
    asset_fk_id  varchar(36) not null,
    primary key (id),
    constraint fk_asset_version_asset
        foreign key (asset_fk_id)
            references assets (id)
            on delete cascade
);

create table if not exists documents
(
    id            varchar(36) not null unique,
    created       timestamp,
    document_id   varchar(50),
    form_path     varchar(200),
    configuration text,
    primary key (id)
);

create table if not exists document_version
(
    id             varchar(36) not null unique,
    created        timestamp,
    language       varchar(20),
    version        bigint,
    status         varchar(20),
    document_fk_id varchar(36) not null,
    primary key (id),
    constraint fk_document_version_document
        foreign key (document_fk_id)
            references documents (id)
            on delete cascade
);

create table if not exists ownerships
(
    id             varchar(36) not null unique,
    created        timestamp,
    owner_id       varchar(50),
    owner_type     varchar(20),
    ownership_type varchar(20),
    object_id      varchar(50),
    object_type    varchar(20),
    primary key (id)
);

create table if not exists relations
(
    id         varchar(36) not null unique,
    created    timestamp,
    relates_to varchar(50),
    object_id  varchar(50),
    object_type varchar(20),
    primary key (id)
);

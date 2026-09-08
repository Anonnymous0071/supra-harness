//! The gateway's tables in the shared store file.
//!
//! The third `schema_component` owner (T11 set the pattern, T16.6 followed
//! it): `user_version` belongs to T10, so the manifest cache records its
//! own history under `"mcp"`, forward only, dense from 1.
//!
//! The table holds one row per discovered tool per server. The cache
//! exists for I3's "probed once" discipline: a session with no network
//! starts from the cache, a session that probes appends what it learns,
//! and the frozen manifest the model saw is never rewritten - a server
//! whose tools changed between sessions adds rows; it never edits or
//! deletes old ones, because a manifest that shrank would break the
//! prefix the same way a rewritten one does.

use supra_store::ComponentMigration;

/// This component's name in `schema_component`.
pub const COMPONENT: &str = "mcp";

/// The cache table's name, for the queries this module runs.
const TABLE: &str = "mcp_tool";

/// The gateway's schema steps, in order.
pub const MIGRATIONS: &[ComponentMigration] = &[ComponentMigration {
    version: 1,
    sql: "
        -- One row per discovered tool per server. Append-only: `probe`
        -- inserts rows that are not present and leaves rows that are -
        -- a tool that vanished upstream keeps its row, because removing
        -- it would mutate the manifest the session promised was frozen.
        CREATE TABLE mcp_tool (
            -- The namespaced tool name: `server__tool`, the name the
            -- registry registers and the manifest shows. Unique per row:
            -- two servers offering the same local name get different
            -- namespaces.
            tool        TEXT PRIMARY KEY NOT NULL
                        CHECK (tool LIKE '%\\_\\_%' ESCAPE '\\'
                               AND length(tool) <= 128),
            -- The server's configured name (the namespace source).
            server      TEXT NOT NULL
                        CHECK (length(server) > 0 AND length(server) <= 64),
            -- The tool's local name as the server reported it.
            local_name  TEXT NOT NULL
                        CHECK (length(local_name) > 0 AND length(local_name) <= 64),
            -- The tool's description, verbatim from the server.
            description TEXT NOT NULL
                        CHECK (length(description) <= 4096),
            -- The tool's input schema, as JSON text, verbatim. The gateway
            -- does not interpret remote schemas; it stores and renders.
            input_schema TEXT NOT NULL
                        CHECK (length(input_schema) <= 65536),
            -- Whether this row came from a live probe or an earlier
            -- session's cache. A live probe that finds the server gone
            -- marks nothing - the rows stay, and `stale` is the TUI's
            -- word for them, not a column that changes behaviour.
            live        INTEGER NOT NULL CHECK (live IN (0, 1))
        ) STRICT;

        CREATE INDEX mcp_tool_server ON mcp_tool (server);
    ",
}];

/// One cached tool row, as the gateway reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedTool {
    /// The namespaced tool name.
    pub tool: String,
    /// The server's name.
    pub server: String,
    /// The local name the server used.
    pub local_name: String,
    /// The description.
    pub description: String,
    /// The input schema, JSON text.
    pub input_schema: String,
    /// Whether the row was written by a live probe in this session.
    pub live: bool,
}

/// Insert one tool row if absent; report whether it was inserted.
///
/// Append-only: an existing row is left untouched, so a server that
/// changed a description between sessions appends nothing and edits
/// nothing - the cached row the manifest promised stays byte-identical.
pub(super) fn append(
    transaction: &rusqlite::Transaction<'_>,
    tool: &CachedTool,
) -> Result<bool, supra_store::StoreError> {
    let inserted = transaction
        .prepare_cached(&format!(
            "INSERT OR IGNORE INTO {TABLE} (tool, server, local_name, description, input_schema, live) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        ))?
        .execute(rusqlite::params![
            tool.tool,
            tool.server,
            tool.local_name,
            tool.description,
            tool.input_schema,
            i64::from(tool.live),
        ])?;
    Ok(inserted == 1)
}

/// Every cached row, ordered by tool name - the order the manifest lists
/// them in, so the manifest is byte-stable across sessions with the same
/// cache.
pub(super) fn all(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<Vec<CachedTool>, supra_store::StoreError> {
    let mut statement = transaction.prepare_cached(&format!(
        "SELECT tool, server, local_name, description, input_schema, live FROM {TABLE} ORDER BY tool"
    ))?;
    let rows = statement.query_map([], decode_row)?;
    let mut tools = Vec::new();
    for row in rows {
        tools.push(row.map_err(|error| supra_store::StoreError::Malformed { detail: error.to_string() })?);
    }
    Ok(tools)
}

/// How many rows the cache holds, per server - the discovery budget's
/// other half.
pub(super) fn count_for_server(
    transaction: &rusqlite::Transaction<'_>,
    server: &str,
) -> Result<i64, supra_store::StoreError> {
    let count: i64 = transaction
        .prepare_cached(&format!("SELECT COUNT(*) FROM {TABLE} WHERE server = ?1"))?
        .query_row([server], |row| row.get(0))?;
    Ok(count)
}

fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedTool> {
    Ok(CachedTool {
        tool: row.get(0)?,
        server: row.get(1)?,
        local_name: row.get(2)?,
        description: row.get(3)?,
        input_schema: row.get(4)?,
        live: row.get::<_, i64>(5)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use supra_store::Store;

    use super::*;

    fn store() -> Store {
        let store = Store::open_in_memory().expect("store");
        store.migrate_component(COMPONENT, MIGRATIONS).expect("migrate");
        store
    }

    #[test]
    fn append_inserts_once_and_never_edits() {
        let store = store();
        let tool = CachedTool {
            tool: "srv__echo".to_owned(),
            server: "srv".to_owned(),
            local_name: "echo".to_owned(),
            description: "Echo.".to_owned(),
            input_schema: "{}".to_owned(),
            live: true,
        };

        store
            .with_transaction(rusqlite::TransactionBehavior::Immediate, |tx| {
                assert!(append(tx, &tool).expect("append"), "first append inserts");
                Ok::<_, supra_store::StoreError>(())
            })
            .expect("tx");

        // A later probe with a *changed* description appends nothing and
        // edits nothing - the cached row is the manifest's promise.
        let mut changed = tool.clone();
        changed.description = "A new description the server sent later.".to_owned();
        store
            .with_transaction(rusqlite::TransactionBehavior::Immediate, |tx| {
                assert!(!append(tx, &changed).expect("append"), "second append is a no-op");
                Ok::<_, supra_store::StoreError>(())
            })
            .expect("tx");

        store
            .with_transaction(rusqlite::TransactionBehavior::Deferred, |tx| {
                let all = all(tx).expect("all");
                assert_eq!(all.len(), 1);
                assert_eq!(all[0].description, "Echo.", "the first row stands unchanged");
                Ok::<_, supra_store::StoreError>(())
            })
            .expect("tx");
    }

    #[test]
    fn the_schema_refuses_names_without_a_namespace() {
        // `server__tool` is the only shape the table accepts: a bare name
        // has no `__`, and a colliding name from two servers is the thing
        // namespacing exists to prevent.
        let connection = rusqlite::Connection::open_in_memory().expect("memory");
        for step in MIGRATIONS {
            connection.execute_batch(step.sql).expect("migrate");
        }
        let refused = connection
            .execute(
                &format!(
                    "INSERT INTO {TABLE} (tool, server, local_name, description, input_schema, live) \
                     VALUES ('echo', 'srv', 'echo', 'd', '{{}}', 1)"
                ),
                [],
            )
            .is_err();
        assert!(refused, "a name without __ must be refused");
    }

    #[test]
    fn the_cache_orders_by_tool_for_a_stable_manifest() {
        let store = store();
        for name in ["zeta__a", "alpha__z", "mid__m"] {
            store
                .with_transaction(rusqlite::TransactionBehavior::Immediate, |tx| {
                    append(
                        tx,
                        &CachedTool {
                            tool: name.to_owned(),
                            server: name.split("__").next().unwrap_or_default().to_owned(),
                            local_name: "t".to_owned(),
                            description: String::new(),
                            input_schema: "{}".to_owned(),
                            live: false,
                        },
                    )
                    .map(|_| ())
                })
                .expect("tx");
        }
        store
            .with_transaction(rusqlite::TransactionBehavior::Deferred, |tx| {
                let names: Vec<String> = all(tx).expect("all").into_iter().map(|t| t.tool).collect();
                assert_eq!(names, vec!["alpha__z", "mid__m", "zeta__a"], "ORDER BY tool");
                Ok::<_, supra_store::StoreError>(())
            })
            .expect("tx");
    }
}

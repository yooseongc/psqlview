use tokio_postgres::Client;

use super::DbError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    Table,
    View,
    MaterializedView,
    Partitioned,
    Foreign,
    Other,
}

impl RelationKind {
    fn from_relkind(c: &str) -> Self {
        match c {
            "r" => Self::Table,
            "v" => Self::View,
            "m" => Self::MaterializedView,
            "p" => Self::Partitioned,
            "f" => Self::Foreign,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "mview",
            Self::Partitioned => "part",
            Self::Foreign => "foreign",
            Self::Other => "?",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Relation {
    pub name: String,
    pub kind: RelationKind,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

pub async fn list_databases(client: &Client) -> Result<Vec<String>, DbError> {
    let rows = client
        .query(
            "SELECT datname FROM pg_catalog.pg_database \
             WHERE NOT datistemplate ORDER BY datname",
            &[],
        )
        .await?;
    Ok(rows.into_iter().map(|r| r.get::<_, String>(0)).collect())
}

pub async fn list_schemas(client: &Client) -> Result<Vec<String>, DbError> {
    let rows = client
        .query(
            "SELECT schema_name FROM information_schema.schemata \
             WHERE schema_name !~ '^pg_' AND schema_name <> 'information_schema' \
             ORDER BY schema_name",
            &[],
        )
        .await?;
    Ok(rows.into_iter().map(|r| r.get::<_, String>(0)).collect())
}

pub async fn list_relations(client: &Client, schema: &str) -> Result<Vec<Relation>, DbError> {
    let rows = client
        .query(
            "SELECT c.relname, c.relkind::text \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r','v','m','p','f') \
             ORDER BY c.relname",
            &[&schema],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let name: String = r.get(0);
            let kind: String = r.get(1);
            Relation {
                name,
                kind: RelationKind::from_relkind(&kind),
            }
        })
        .collect())
}

pub async fn list_columns(
    client: &Client,
    schema: &str,
    table: &str,
) -> Result<Vec<Column>, DbError> {
    let rows = client
        .query(
            "SELECT column_name, data_type, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 \
             ORDER BY ordinal_position",
            &[&schema, &table],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let is_nullable: String = r.get(2);
            Column {
                name: r.get(0),
                data_type: r.get(1),
                nullable: is_nullable == "YES",
                default: r.get(3),
            }
        })
        .collect())
}

/// One row of the DDL-shaping query: a part of the relation's definition,
/// tagged so the assembler knows whether it goes inside the parens or
/// out. `kind` is `"col"`, `"con"`, or `"idx"`; `def` is the rendered
/// piece (column line, constraint def, full `CREATE INDEX` statement).
/// Ordering comes from the SQL — the assembler walks rows in receive
/// order.
#[derive(Debug, Clone)]
struct DdlPart {
    kind: String,
    def: String,
}

/// Fetches a synthetic `CREATE TABLE` definition for `schema.relation`,
/// plus any standalone `CREATE INDEX` statements. Compatible with
/// PostgreSQL 14+ (uses only `pg_attribute`, `pg_attrdef`, `pg_class`,
/// `pg_namespace`, `pg_constraint`, `pg_indexes`, `format_type`,
/// `pg_get_expr`, `pg_get_constraintdef`).
pub async fn fetch_table_ddl(
    client: &Client,
    schema: &str,
    relation: &str,
) -> Result<String, DbError> {
    let rows = client
        .query(
            r#"
WITH cls AS (
    SELECT c.oid, c.relkind FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = $1 AND c.relname = $2
),
cols AS (
    SELECT a.attnum AS ord,
           format(
               '%I %s%s%s',
               a.attname,
               format_type(a.atttypid, a.atttypmod),
               CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END,
               COALESCE(' DEFAULT ' || pg_get_expr(d.adbin, d.adrelid), '')
           ) AS def
    FROM pg_catalog.pg_attribute a
    JOIN cls ON cls.oid = a.attrelid
    LEFT JOIN pg_catalog.pg_attrdef d
        ON d.adrelid = a.attrelid AND d.adnum = a.attnum
    WHERE a.attnum > 0 AND NOT a.attisdropped
),
cons AS (
    SELECT 0 AS ord,
           pg_get_constraintdef(oid) AS def,
           CASE contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'f' THEN 2 ELSE 3 END AS pri
    FROM pg_catalog.pg_constraint
    WHERE conrelid = (SELECT oid FROM cls)
)
SELECT 'col' AS kind, ord, def FROM cols
UNION ALL
SELECT 'con', pri, def FROM cons
UNION ALL
SELECT 'idx', 0, indexdef FROM pg_catalog.pg_indexes
WHERE schemaname = $1 AND tablename = $2 AND indexname NOT IN (
    SELECT conname FROM pg_catalog.pg_constraint WHERE conrelid = (SELECT oid FROM cls)
)
ORDER BY 1, 2;
"#,
            &[&schema, &relation],
        )
        .await?;

    if rows.is_empty() {
        return Err(DbError::Other(format!(
            "relation not found: {schema}.{relation}"
        )));
    }

    let parts: Vec<DdlPart> = rows
        .into_iter()
        .map(|r| DdlPart {
            kind: r.get(0),
            def: r.get(2),
        })
        .collect();

    Ok(assemble_ddl(schema, relation, &parts))
}

/// Pure DDL assembler — pulled out for unit testing.
fn assemble_ddl(schema: &str, relation: &str, parts: &[DdlPart]) -> String {
    let mut columns: Vec<&str> = Vec::new();
    let mut constraints: Vec<&str> = Vec::new();
    let mut indexes: Vec<&str> = Vec::new();
    for p in parts {
        match p.kind.as_str() {
            "col" => columns.push(&p.def),
            "con" => constraints.push(&p.def),
            "idx" => indexes.push(&p.def),
            _ => {}
        }
    }
    let mut out = String::new();
    out.push_str(&format!(
        "CREATE TABLE {}.{} (\n",
        quote_pg_ident(schema),
        quote_pg_ident(relation)
    ));
    let last = columns.len() + constraints.len();
    let mut written = 0usize;
    for c in &columns {
        written += 1;
        out.push_str("    ");
        out.push_str(c);
        if written < last {
            out.push(',');
        }
        out.push('\n');
    }
    for c in &constraints {
        written += 1;
        out.push_str("    ");
        out.push_str(c);
        if written < last {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(");\n");
    for ix in &indexes {
        out.push('\n');
        out.push_str(ix);
        out.push_str(";\n");
    }
    out
}

fn quote_pg_ident(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(kind: &str, def: &str) -> DdlPart {
        DdlPart {
            kind: kind.to_string(),
            def: def.to_string(),
        }
    }

    #[test]
    fn assemble_ddl_emits_columns_and_constraints_inside_parens() {
        let parts = vec![
            p("col", "id integer NOT NULL"),
            p("col", "email text"),
            p("con", "PRIMARY KEY (id)"),
        ];
        let out = assemble_ddl("public", "users", &parts);
        assert!(out.starts_with("CREATE TABLE \"public\".\"users\" (\n"));
        assert!(out.contains("    id integer NOT NULL,\n"));
        assert!(out.contains("    email text,\n"));
        assert!(out.contains("    PRIMARY KEY (id)\n"));
        assert!(out.contains(");\n"));
    }

    #[test]
    fn assemble_ddl_omits_trailing_comma_on_last_member() {
        let parts = vec![p("col", "id integer NOT NULL")];
        let out = assemble_ddl("public", "t", &parts);
        assert!(out.contains("    id integer NOT NULL\n"));
        assert!(!out.contains("    id integer NOT NULL,\n"));
    }

    #[test]
    fn assemble_ddl_appends_indexes_outside_parens() {
        let parts = vec![
            p("col", "id integer"),
            p(
                "idx",
                "CREATE INDEX users_email_idx ON public.users (email)",
            ),
        ];
        let out = assemble_ddl("public", "users", &parts);
        assert!(out.contains("CREATE TABLE \"public\".\"users\""));
        assert!(out.contains("CREATE INDEX users_email_idx"));
        let paren_close = out.find(");\n").unwrap();
        let index_pos = out.find("CREATE INDEX").unwrap();
        assert!(index_pos > paren_close);
    }

    #[test]
    fn quote_pg_ident_doubles_internal_quotes() {
        assert_eq!(quote_pg_ident("a"), "\"a\"");
        assert_eq!(quote_pg_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn relation_kind_maps_all_supported_letters() {
        assert_eq!(RelationKind::from_relkind("r"), RelationKind::Table);
        assert_eq!(RelationKind::from_relkind("v"), RelationKind::View);
        assert_eq!(
            RelationKind::from_relkind("m"),
            RelationKind::MaterializedView
        );
        assert_eq!(RelationKind::from_relkind("p"), RelationKind::Partitioned);
        assert_eq!(RelationKind::from_relkind("f"), RelationKind::Foreign);
        assert_eq!(RelationKind::from_relkind("z"), RelationKind::Other);
    }
}

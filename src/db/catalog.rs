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

#[cfg(test)]
mod tests {
    use super::*;

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

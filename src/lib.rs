//! PostGIS: add a PostgreSQL/PostGIS table to the map as a layer.
//!
//! A plugin runs in a sandbox with no network of its own, and RustGIS has no PostgreSQL driver,
//! so the plugin asks RustGIS to run a DuckDB query (`layer.addSql`) that reads the table through
//! DuckDB's `postgres` extension (`postgres_scan`). RustGIS adds the result as an ordinary vector
//! layer: attribute table, symbology, selection and geoprocessing all work on it.
//!
//! - DuckDB 1.5 or later must be installed for RustGIS (the `duckdb` program on the PATH, or
//!   `RUSTGIS_DUCKDB_BIN`). DuckDB downloads its `postgres` extension the first time.
//! - `layer.addSql` reads files and the network, so the plugin needs the `files` and `network`
//!   permissions, and `edit` to add the layer.
//! - The connection is a libpq connection string (`host=… dbname=… user=…`), a
//!   `postgresql://` URI, or `service=<name>` from `~/.pg_service.conf`. Passwords belong in
//!   `~/.pgpass` or the service file: the query, connection string included, is passed to `duckdb`
//!   on its command line and saved with the project when the layer streams.
//!
//! The ribbon and command search run a command without parameters, and a plugin can't open a
//! dialog. So `add-table` without a `table` works from a "PostGIS Sources" table in the project:
//! the first time it adds the table for the user to fill in (Attribute Table), and after that it
//! adds the table's rows (the selected ones, when some are selected) as layers.

use rustgis_plugin::{Kind, Map, Request, ToolOutput, Value, geojson, host, json};

/// The standalone table that holds the sources the ribbon button adds.
pub const SOURCES: &str = "PostGIS Sources";
/// Its columns: `add-table`'s parameters, as text.
pub const SOURCE_FIELDS: [&str; 8] = ["connection", "table", "name", "columns", "where", "limit", "crs", "geometry"];

fn handle(req: Request) -> Result<Value, String> {
    match (req.kind, req.id.as_str()) {
        (Kind::Tool, "set-source") => set_source(&req),
        (Kind::Command, "add-table") if req.params.get("table").is_some() => add_table(&req.params),
        (Kind::Command, "add-table") => add_sources(),
        (_, other) => Err(format!("PostGIS has no command '{other}'")),
    }
}

rustgis_plugin::export!(handle);

/// What `add-table` reads, from its parameters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableQuery {
    /// A libpq connection string, URI or `service=…`; empty uses libpq's defaults (PG* variables).
    pub connection: String,
    pub schema: String,
    pub table: String,
    /// Columns to read (all when empty).
    pub columns: Vec<String>,
    /// The geometry column the layer draws, when the table has more than one.
    pub geometry: Option<String>,
    /// A DuckDB SQL condition on the table's columns.
    pub filter: Option<String>,
    pub limit: Option<u64>,
}

impl TableQuery {
    /// Reads `add-table`'s parameters.
    pub fn from_params(p: &Value) -> Result<TableQuery, String> {
        let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let (schema, table) = parse_table(text("table").ok_or("'table' is required: the table as schema.table")?)?;
        let columns = match p.get("columns") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => s.split(',').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect(),
            Some(Value::Array(a)) => {
                a.iter().map(|c| c.as_str().map(String::from).ok_or("'columns' must be a list of column names")).collect::<Result<_, _>>()?
            }
            Some(_) => return Err("'columns' must be a list of column names".into()),
        };
        let limit = match p.get("limit") {
            None | Some(Value::Null) => None,
            // An empty cell of the Sources table.
            Some(Value::String(s)) if s.trim().is_empty() => None,
            Some(v) => Some(
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    .ok_or("'limit' must be a whole number of rows")?,
            ),
        };
        Ok(TableQuery {
            connection: text("connection").unwrap_or_default().to_string(),
            schema,
            table,
            columns,
            geometry: text("geometry").map(String::from),
            filter: text("where").map(String::from),
            limit,
        })
    }

    /// The DuckDB query that reads the table.
    pub fn sql(&self) -> String {
        let mut select: Vec<String> = Vec::new();
        // RustGIS draws the first geometry column, so the chosen one goes first.
        if let Some(g) = &self.geometry {
            select.push(ident(g));
        }
        if self.columns.is_empty() {
            select.push(match &self.geometry {
                Some(g) => format!("* EXCLUDE ({})", ident(g)),
                None => "*".to_string(),
            });
        } else {
            select.extend(self.columns.iter().filter(|c| Some(*c) != self.geometry.as_ref()).map(|c| ident(c)));
        }
        let mut sql = format!(
            "SELECT {} FROM postgres_scan({}, {}, {})",
            select.join(", "),
            literal(&self.connection),
            literal(&self.schema),
            literal(&self.table)
        );
        if let Some(f) = &self.filter {
            sql.push_str(&format!(" WHERE ({f})"));
        }
        if let Some(n) = self.limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        sql
    }
}

/// `schema.table` (or `table`, in `public`) as PostgreSQL reads it: unquoted names in lower case,
/// `"Quoted"` names as written.
pub fn parse_table(s: &str) -> Result<(String, String), String> {
    let mut parts: Vec<String> = Vec::new();
    let mut chars = s.trim().chars().peekable();
    loop {
        let mut part = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') if chars.peek() == Some(&'"') => {
                        chars.next();
                        part.push('"');
                    }
                    Some('"') => break,
                    Some(c) => part.push(c),
                    None => return Err(format!("'{s}': a quoted name isn't closed")),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == '.' {
                    break;
                }
                part.push(c.to_ascii_lowercase());
                chars.next();
            }
            part = part.trim().to_string();
        }
        if part.is_empty() {
            return Err(format!("'{s}' isn't a table name: write schema.table"));
        }
        parts.push(part);
        match chars.next() {
            None => break,
            Some('.') => continue,
            Some(_) => return Err(format!("'{s}' isn't a table name: write schema.table")),
        }
    }
    match <[String; 2]>::try_from(parts) {
        Ok([schema, table]) => Ok((schema, table)),
        Err(parts) if parts.len() == 1 => Ok(("public".to_string(), parts.into_iter().next().unwrap_or_default())),
        Err(_) => Err(format!("'{s}' isn't a table name: write schema.table")),
    }
}

/// A DuckDB string literal.
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A DuckDB quoted identifier.
fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Adds the table `p` (`add-table`'s parameters) names as a layer.
fn add_table(p: &Value) -> Result<Value, String> {
    let q = TableQuery::from_params(p)?;
    let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let name = text("name").unwrap_or(&q.table).to_string();
    let mut params = json!({"sql": q.sql(), "name": name});
    if let Some(crs) = text("crs") {
        params["crs"] = json!(crs);
    }
    let added = host::run("layer.addSql", params).map_err(|e| explain(&e, &q))?;
    if p.get("zoom").and_then(Value::as_bool).unwrap_or(true) {
        // By id: a table mixing geometry types becomes "<name> (points)", "<name> (lines)"…
        let _ = host::run("layer.zoomTo", json!({"layer": added.get("layer").cloned().unwrap_or(json!(name))}));
    }
    let n = added.get("features").and_then(Value::as_u64).unwrap_or(0);
    let mut msg = format!("Added {}.{} as '{name}' ({n} features)", q.schema, q.table);
    if q.connection.to_ascii_lowercase().contains("password") || has_uri_password(&q.connection) {
        msg.push_str(". The connection holds a password, which RustGIS can save in the project: use ~/.pgpass or ~/.pg_service.conf");
    }
    host::notify(&msg);
    Ok(added)
}

/// Whether a `postgresql://user:password@host` URI carries a password.
fn has_uri_password(conn: &str) -> bool {
    conn.split_once("://")
        .and_then(|(_, rest)| rest.split(['/', '?']).next())
        .and_then(|auth| auth.rsplit_once('@'))
        .is_some_and(|(user, _)| user.contains(':'))
}

/// `add-table` without parameters: adds the rows of the PostGIS Sources table (the selected
/// ones, when some are), or adds that table for the user to fill in.
fn add_sources() -> Result<Value, String> {
    let layers = host::layers()?;
    // The newest one: each run of the Set Source tool adds another.
    let Some(sources) = layers.as_array().and_then(|a| a.iter().filter(|l| is_sources(l["name"].as_str().unwrap_or_default())).max_by_key(|l| l["id"].as_u64())) else {
        return new_sources();
    };
    let id = sources["id"].clone();
    let selected = sources["selected"].as_u64().unwrap_or(0) > 0 || sources["selected"] == true;
    let fc = host::call("host.features", json!({"layer": id, "selected": selected}))?;
    let rows = source_rows(&fc);
    if rows.is_empty() {
        open_sources();
        return Err(format!("Fill in a row of '{SOURCES}' (connection, and table as schema.table), then run Add PostGIS Table again"));
    }
    let (mut added, mut failed) = (Vec::new(), Vec::new());
    for (row, mut p) in rows.iter().cloned() {
        p.insert("zoom".into(), json!(rows.len() == 1));
        let table = p.get("table").cloned().unwrap_or_default();
        match add_table(&Value::Object(p)) {
            Ok(v) => added.push(json!({"row": row, "table": table, "result": v})),
            Err(e) => failed.push(json!({"row": row, "table": table, "error": e})),
        }
    }
    // A failure would roll the whole command back, the layers that worked included, so the
    // command fails only when nothing was added.
    if added.is_empty() {
        let errors: Vec<&str> = failed.iter().filter_map(|f| f["error"].as_str()).collect();
        return Err(errors.join("\n"));
    }
    if !failed.is_empty() {
        let tables: Vec<String> = failed.iter().map(|f| f["table"].as_str().unwrap_or("?").to_string()).collect();
        let _ = host::call(
            "host.notify",
            json!({"text": format!("Added {} of {} PostGIS sources; failed: {}", added.len(), rows.len(), tables.join(", ")), "error": true}),
        );
    }
    Ok(json!({"added": added, "failed": failed}))
}

/// The filled-in rows of the Sources table: (row, parameters), skipping rows without a table.
pub fn source_rows(fc: &Value) -> Vec<(u64, Map<String, Value>)> {
    let features = fc.get("features").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    features
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let props = f.get("properties")?.as_object()?;
            let p: Map<String, Value> =
                SOURCE_FIELDS.iter().filter_map(|k| props.get(*k).filter(|v| !v.is_null()).map(|v| (k.to_string(), v.clone()))).collect();
            let has_table = p.get("table").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty());
            let row = f.get("row").or_else(|| props.get("row")).and_then(Value::as_u64).unwrap_or(i as u64);
            has_table.then_some((row, p))
        })
        .collect()
}

/// Whether a layer name is a Sources table: RustGIS numbers repeats ("PostGIS Sources (2)").
pub fn is_sources(name: &str) -> bool {
    name == SOURCES || name.strip_prefix(SOURCES).and_then(|r| r.strip_prefix(" (")).and_then(|r| r.strip_suffix(')')).is_some_and(|n| n.parse::<u32>().is_ok())
}

/// The Set Source tool: a form (connection, table…) that makes a one-row Sources layer, for
/// Add PostGIS Table to read. The attribute table's cell editor can get stuck, a form can't.
fn set_source(req: &Request) -> Result<Value, String> {
    let row: Map<String, Value> =
        SOURCE_FIELDS.iter().map(|k| (k.to_string(), json!(req.str(k).map(str::trim).unwrap_or_default()))).collect();
    let p = Value::Object(row.clone());
    // Checked now, so a typo shows here and not after a trip to the database.
    TableQuery::from_params(&p)?;
    let fc = geojson::collection(vec![geojson::feature(Value::Null, Value::Object(row))]);
    Ok(ToolOutput::new().layer(SOURCES, fc).message("Now run Plugins › PostGIS › Add PostGIS Table").into())
}

/// Opens the Sources table's attribute table (where a double-click edits a cell).
fn open_sources() {
    let _ = host::run("layer.select", json!({"layer": SOURCES}));
    let _ = host::run("table.open", json!({"layer": SOURCES}));
}

/// Adds the PostGIS Sources table, one empty row to fill in, and opens it.
fn new_sources() -> Result<Value, String> {
    let mut row: Map<String, Value> = SOURCE_FIELDS.iter().map(|k| (k.to_string(), json!(""))).collect();
    row.insert("connection".into(), json!("host=localhost port=5432 dbname=postgres user=postgres"));
    let fc = geojson::collection(vec![json!({"type": "Feature", "geometry": null, "properties": row})]);
    let added = host::add_layer(SOURCES, fc)?;
    open_sources();
    host::notify(&format!(
        "Added the '{SOURCES}' table: fill in a row (connection, and table as schema.table), then run Add PostGIS Table again"
    ));
    Ok(json!({"created": SOURCES, "layer": added.get("layer")}))
}

/// Adds what to do about the errors people run into first.
pub fn explain(e: &str, q: &TableQuery) -> String {
    let hint = if e.contains("need DuckDB") {
        "Install DuckDB 1.5 or later (brew install duckdb) and restart RustGIS.".to_string()
    } else if e.contains("No such file or directory") {
        // What RustGIS reports when a DuckDB older than 1.5 runs the query and writes nothing.
        "RustGIS needs DuckDB 1.5 or later: check `duckdb --version` and upgrade it (brew upgrade duckdb).".to_string()
    } else if e.contains("Unable to connect to Postgres") {
        "Check the connection's host, dbname and user, with the password in ~/.pgpass or a service in ~/.pg_service.conf."
            .to_string()
    } else if e.contains("does not contain any columns") {
        format!("There is no table {}.{} that this user can read.", q.schema, q.table)
    } else if e.contains("no geometry column") {
        "The table has no geometry column, or 'columns' left it out.".to_string()
    } else {
        return e.to_string();
    };
    format!("{}. {hint}", e.trim_end().trim_end_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(p: Value) -> TableQuery {
        TableQuery::from_params(&p).unwrap()
    }

    #[test]
    fn parses_table_names_as_postgres_does() {
        let t = |s: &str| parse_table(s).unwrap();
        assert_eq!(t("public.counties"), ("public".into(), "counties".into()));
        assert_eq!(t("Parcels"), ("public".into(), "parcels".into()));
        assert_eq!(t(" Publish . Parcels "), ("publish".into(), "parcels".into()));
        assert_eq!(t(r#""My Schema"."Odd.Name""#), ("My Schema".into(), "Odd.Name".into()));
        assert_eq!(t(r#"s."say ""hi""""#), ("s".into(), r#"say "hi""#.into()));
        for bad in ["", ".", "a.", ".b", "a.b.c", r#""open"#, r#""a"x.b"#] {
            assert!(parse_table(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn builds_the_duckdb_query() {
        let q = query(json!({"table": "public.counties", "connection": "service=mydb"}));
        assert_eq!(q.sql(), "SELECT * FROM postgres_scan('service=mydb', 'public', 'counties')");

        let q = query(json!({
            "table": "public.counties", "connection": "host=mydb password='it''s'",
            "columns": ["geoid", "county_name", "geom"], "where": "statefp = '13'", "limit": "10",
        }));
        assert_eq!(
            q.sql(),
            "SELECT \"geoid\", \"county_name\", \"geom\" FROM postgres_scan('host=mydb password=''it''''s''', \
             'public', 'counties') WHERE (statefp = '13') LIMIT 10"
        );
    }

    #[test]
    fn puts_the_chosen_geometry_first() {
        let q = query(json!({"table": "t", "geometry": "centroid"}));
        assert_eq!(q.sql(), "SELECT \"centroid\", * EXCLUDE (\"centroid\") FROM postgres_scan('', 'public', 't')");
        let q = query(json!({"table": "t", "geometry": "geom", "columns": "id, geom"}));
        assert_eq!(q.sql(), "SELECT \"geom\", \"id\" FROM postgres_scan('', 'public', 't')");
    }

    #[test]
    fn rejects_bad_parameters() {
        for p in [json!({}), json!({"table": " "}), json!({"table": "t", "limit": -1}), json!({"table": "t", "columns": 3})] {
            assert!(TableQuery::from_params(&p).is_err(), "{p}");
        }
    }

    #[test]
    fn explains_common_failures() {
        let q = query(json!({"table": "publish.missing"}));
        assert_eq!(
            explain("DuckDB: Invalid Input Error: Table missing does not contain any columns.", &q),
            "DuckDB: Invalid Input Error: Table missing does not contain any columns. There is no table publish.missing that this user can read."
        );
        assert!(explain("i/o: No such file or directory (os error 2)", &q).contains("DuckDB 1.5 or later"));
        assert!(explain("Unable to connect to Postgres at \"\": no password", &q).ends_with("~/.pg_service.conf."));
        assert_eq!(explain("something else", &q), "something else");
    }

    #[test]
    fn reads_the_sources_table() {
        let fc = json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "geometry": null, "row": 0, "properties": {"connection": "service=mydb", "table": "", "limit": ""}},
            {"type": "Feature", "geometry": null, "row": 1, "properties": {
                "connection": "service=mydb", "table": "public.counties", "name": "", "limit": "", "where": null, "other": 1}},
        ]});
        let rows = source_rows(&fc);
        assert_eq!(rows.len(), 1);
        let (row, p) = &rows[0];
        assert_eq!(*row, 1);
        assert!(p.get("other").is_none() && p.get("where").is_none());
        // Empty cells read as unset parameters.
        let q = TableQuery::from_params(&Value::Object(p.clone())).unwrap();
        assert_eq!(q.sql(), "SELECT * FROM postgres_scan('service=mydb', 'public', 'counties')");
    }

    #[test]
    fn finds_numbered_sources_layers() {
        assert!(is_sources("PostGIS Sources") && is_sources("PostGIS Sources (12)"));
        assert!(!is_sources("PostGIS Sources (x)") && !is_sources("PostGIS Sourcesx") && !is_sources("Counties"));
    }

    #[test]
    fn spots_passwords_in_uris() {
        assert!(has_uri_password("postgresql://me:secret@mydb/gis"));
        assert!(!has_uri_password("postgresql://me@mydb/gis"));
        assert!(!has_uri_password("postgresql://mydb:5432/gis"));
        assert!(!has_uri_password("host=mydb user=me"));
    }

    #[test]
    fn manifest_declares_what_the_plugin_uses() {
        let m: Value = rustgis_plugin::serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(m["permissions"], json!(["edit", "files", "network"]));
        let ids: Vec<&str> = m["commands"].as_array().unwrap().iter().filter_map(|c| c["id"].as_str()).collect();
        assert_eq!(ids, ["add-table"]);
    }
}

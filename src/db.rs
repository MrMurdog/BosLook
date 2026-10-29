//! Speicher für Telegramme und Beschreibungen: SQLite (Datei), MySQL/MariaDB oder PostgreSQL.
//!
//! Die Schnittstelle ist synchron (die Datenbank liegt hinter einem Mutex); die Abfragen laufen
//! über sqlx in einer eigenen kleinen Tokio-Laufzeit.

use crate::descriptions::Description;
use crate::telegram::{Kind, Telegram};
use anyhow::Result;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{MySqlPool, PgPool, QueryBuilder, SqlitePool};
use std::borrow::Cow;

enum Pool {
    Sqlite(SqlitePool),
    MySql(MySqlPool),
    Postgres(PgPool),
}

pub struct Db {
    pool: Pool,
    rt: Option<tokio::runtime::Runtime>,
}

/// Führt `$body` mit der jeweiligen Verbindung aus; der Code ist für alle Datenbanken gleich.
macro_rules! run {
    ($db:expr, |$pool:ident| $body:expr) => {
        $db.block(async {
            match &$db.pool {
                Pool::Sqlite($pool) => $body,
                Pool::MySql($pool) => $body,
                Pool::Postgres($pool) => $body,
            }
        })
    };
}

const TELEGRAMS: &str = "id, time, source, kind, address, sub, message, ort, kurz, lang";

/// So viele Zeilen schreibt ein INSERT beim Import auf einmal
const BATCH: usize = 500;

impl Db {
    /// `database` ist ein Dateiname (SQLite) oder eine Adresse `mysql://…` bzw. `postgres://…`.
    pub fn open(database: &str) -> Result<Db> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let pool = block_on(&rt, async {
            anyhow::Ok(if database.starts_with("mysql://") || database.starts_with("mariadb://") {
                Pool::MySql(MySqlPool::connect(&database.replacen("mariadb://", "mysql://", 1)).await?)
            } else if database.starts_with("postgres://") || database.starts_with("postgresql://") {
                Pool::Postgres(PgPool::connect(database).await?)
            } else {
                let options = SqliteConnectOptions::new().filename(database).create_if_missing(true);
                // Eine Verbindung genügt (Zugriff über Mutex) und hält auch ":memory:" zusammen
                Pool::Sqlite(sqlx::pool::PoolOptions::new().max_connections(1).connect_with(options).await?)
            })
        })?;
        let db = Db { pool, rt: Some(rt) };
        db.create_tables()?;
        Ok(db)
    }

    fn create_tables(&self) -> Result<()> {
        let statements: &[&str] = match self.pool {
            Pool::Sqlite(_) => &[
                "CREATE TABLE IF NOT EXISTS telegrams (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    time TEXT NOT NULL,
                    source TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    address TEXT NOT NULL,
                    sub TEXT NOT NULL,
                    message TEXT NOT NULL,
                    ort TEXT NOT NULL DEFAULT '',
                    kurz TEXT NOT NULL DEFAULT '',
                    lang TEXT NOT NULL DEFAULT ''
                )",
                "CREATE INDEX IF NOT EXISTS telegrams_source ON telegrams (source, id)",
                "CREATE TABLE IF NOT EXISTS descriptions (
                    kind TEXT NOT NULL,
                    address TEXT NOT NULL,
                    ort TEXT NOT NULL,
                    kurz TEXT NOT NULL,
                    lang TEXT NOT NULL,
                    PRIMARY KEY (kind, address)
                )",
            ],
            Pool::MySql(_) => &[
                "CREATE TABLE IF NOT EXISTS telegrams (
                    id BIGINT AUTO_INCREMENT PRIMARY KEY,
                    time VARCHAR(40) NOT NULL,
                    source VARCHAR(255) NOT NULL,
                    kind VARCHAR(16) NOT NULL,
                    address VARCHAR(255) NOT NULL,
                    sub VARCHAR(16) NOT NULL,
                    message TEXT NOT NULL,
                    ort TEXT NOT NULL,
                    kurz TEXT NOT NULL,
                    lang TEXT NOT NULL,
                    INDEX telegrams_source (source, id)
                ) DEFAULT CHARSET = utf8mb4",
                "CREATE TABLE IF NOT EXISTS descriptions (
                    kind VARCHAR(16) NOT NULL,
                    address VARCHAR(16) NOT NULL,
                    ort TEXT NOT NULL,
                    kurz TEXT NOT NULL,
                    lang TEXT NOT NULL,
                    PRIMARY KEY (kind, address)
                ) DEFAULT CHARSET = utf8mb4",
            ],
            Pool::Postgres(_) => &[
                "CREATE TABLE IF NOT EXISTS telegrams (
                    id BIGSERIAL PRIMARY KEY,
                    time TEXT NOT NULL,
                    source TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    address TEXT NOT NULL,
                    sub TEXT NOT NULL,
                    message TEXT NOT NULL,
                    ort TEXT NOT NULL,
                    kurz TEXT NOT NULL,
                    lang TEXT NOT NULL
                )",
                "CREATE INDEX IF NOT EXISTS telegrams_source ON telegrams (source, id)",
                "CREATE TABLE IF NOT EXISTS descriptions (
                    kind TEXT NOT NULL,
                    address TEXT NOT NULL,
                    ort TEXT NOT NULL,
                    kurz TEXT NOT NULL,
                    lang TEXT NOT NULL,
                    PRIMARY KEY (kind, address)
                )",
            ],
        };
        run!(self, |pool| {
            for s in statements {
                sqlx::query(s).execute(pool).await?;
            }
            Ok::<_, sqlx::Error>(())
        })?;
        if let Pool::Sqlite(pool) = &self.pool {
            // Datenbanken von BosLook 0.1.4 und älter haben noch keine Beschreibungen
            let columns: Vec<String> = self.block(sqlx::query_scalar("SELECT name FROM pragma_table_info('telegrams')").fetch_all(pool))?;
            for column in ["ort", "kurz", "lang"].into_iter().filter(|c| !columns.iter().any(|x| x == c)) {
                let alter = format!("ALTER TABLE telegrams ADD COLUMN {column} TEXT NOT NULL DEFAULT ''");
                self.block(sqlx::query(&alter).execute(pool))?;
            }
        }
        Ok(())
    }

    fn block<F: Future<Output = Result<T, E>>, T, E: Into<anyhow::Error>>(&self, f: F) -> Result<T> {
        block_on(self.rt.as_ref().expect("Laufzeit"), f).map_err(Into::into)
    }

    /// Platzhalter `?` für PostgreSQL in `$1`, `$2` … umschreiben.
    fn sql<'a>(&self, query: &'a str) -> Cow<'a, str> {
        if !matches!(self.pool, Pool::Postgres(_)) {
            return Cow::Borrowed(query);
        }
        let mut n = 0;
        let mut out = String::with_capacity(query.len() + 8);
        for c in query.chars() {
            if c == '?' {
                n += 1;
                out.push_str(&format!("${n}"));
            } else {
                out.push(c);
            }
        }
        Cow::Owned(out)
    }

    pub fn insert(&self, t: &Telegram) -> Result<i64> {
        let time = t.time.to_rfc3339();
        let values = [&time, &t.source, t.kind.as_str(), &t.address, &t.sub, &t.message, &t.ort, &t.kurz, &t.lang];
        let insert = "INSERT INTO telegrams (time, source, kind, address, sub, message, ort, kurz, lang)
                      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)";
        match &self.pool {
            Pool::MySql(pool) => {
                let query = values.iter().fold(sqlx::query(insert), |q, v| q.bind(*v));
                Ok(self.block(query.execute(pool))?.last_insert_id() as i64)
            }
            Pool::Sqlite(pool) => {
                let sql = insert.to_string() + " RETURNING id";
                let query = values.iter().fold(sqlx::query_scalar(&sql), |q, v| q.bind(*v));
                self.block(query.fetch_one(pool))
            }
            Pool::Postgres(pool) => {
                let sql = self.sql(insert) + " RETURNING id";
                let query = values.iter().fold(sqlx::query_scalar(&sql), |q, v| q.bind(*v));
                self.block(query.fetch_one(pool))
            }
        }
    }

    /// Ob genau dieses Telegramm schon gespeichert ist (unabhängig von Zeit und Quelle).
    pub fn contains(&self, kind: Kind, address: &str, message: &str) -> Result<bool> {
        let sql = self.sql("SELECT kind FROM telegrams WHERE kind = ? AND address = ? AND message = ? LIMIT 1");
        let found: Option<String> = run!(self, |pool| {
            sqlx::query_scalar(&sql).bind(kind.as_str()).bind(address).bind(message).fetch_optional(pool).await
        })?;
        Ok(found.is_some())
    }

    /// Alle Quellen, von denen Telegramme gespeichert sind.
    pub fn sources(&self) -> Result<Vec<String>> {
        run!(self, |pool| sqlx::query_scalar("SELECT DISTINCT source FROM telegrams ORDER BY source").fetch_all(pool).await)
    }

    /// Text eines gespeicherten Telegramms ersetzen (bessere Fassung beim Doppelfilter)
    pub fn update_message(&self, id: i64, message: &str) -> Result<()> {
        let sql = self.sql("UPDATE telegrams SET message = ? WHERE id = ?");
        run!(self, |pool| sqlx::query(&sql).bind(message).bind(id).execute(pool).await.map(|_| ()))
    }

    /// Alle Telegramme einer Quelle löschen; liefert die Anzahl
    pub fn delete_source(&self, source: &str) -> Result<u64> {
        let sql = self.sql("DELETE FROM telegrams WHERE source = ?");
        let done = run!(self, |pool| sqlx::query(&sql).bind(source).execute(pool).await.map(|r| r.rows_affected()))?;
        Ok(done)
    }

    /// Die neuesten Telegramme einer Quelle, optional nur die älteren als `before` (für Blättern).
    pub fn latest(&self, source: &str, limit: u32, before: Option<i64>) -> Result<Vec<Telegram>> {
        let sql = format!("SELECT {TELEGRAMS} FROM telegrams WHERE source = ? AND id < ? ORDER BY id DESC LIMIT ?");
        let sql = self.sql(&sql);
        type Row = (i64, String, String, String, String, String, String, String, String, String);
        let rows: Vec<Row> = run!(self, |pool| {
            sqlx::query_as(&sql).bind(source).bind(before.unwrap_or(i64::MAX)).bind(i64::from(limit)).fetch_all(pool).await
        })?;
        Ok(rows
            .into_iter()
            .map(|(id, time, source, kind, address, sub, message, ort, kurz, lang)| Telegram {
                id,
                time: chrono::DateTime::parse_from_rfc3339(&time)
                    .map(|t| t.with_timezone(&chrono::Local))
                    .unwrap_or_default(),
                source,
                kind: Kind::parse(&kind).unwrap_or(Kind::Pocsag),
                address,
                sub,
                message,
                ort,
                kurz,
                lang,
            })
            .collect())
    }

    /// Ort und Beschreibungen aus dem Adressbuch übernehmen: zuerst die genaue Sub, dann `*`.
    pub fn describe(&self, t: &mut Telegram) -> Result<()> {
        if t.kind != Kind::Pocsag {
            return Ok(());
        }
        let exact = format!("{}{}", t.address, t.sub);
        let wildcard = format!("{}*", t.address);
        let sql = self.sql("SELECT address, ort, kurz, lang FROM descriptions WHERE kind = ? AND address IN (?, ?)");
        let rows: Vec<(String, String, String, String)> = run!(self, |pool| {
            sqlx::query_as(&sql).bind(Kind::Pocsag.as_str()).bind(&exact).bind(&wildcard).fetch_all(pool).await
        })?;
        if let Some((_, ort, kurz, lang)) = rows.iter().find(|r| r.0 == exact).or(rows.first()).cloned() {
            (t.ort, t.kurz, t.lang) = (ort, kurz, lang);
        }
        Ok(())
    }

    /// Alle Beschreibungen (für Editor und Export).
    pub fn descriptions(&self) -> Result<Vec<Description>> {
        let sql = self.sql("SELECT address, ort, kurz, lang FROM descriptions WHERE kind = ?");
        let rows: Vec<(String, String, String, String)> =
            run!(self, |pool| sqlx::query_as(&sql).bind(Kind::Pocsag.as_str()).fetch_all(pool).await)?;
        Ok(rows.into_iter().map(|(address, ort, kurz, lang)| Description { address, ort, kurz, lang }).collect())
    }

    /// Beschreibungen in einer Transaktion löschen (`clear`: alle, sonst die Adressen in `delete`)
    /// und dann `insert` anlegen.
    pub fn write_descriptions(&self, clear: bool, delete: &[&str], insert: &[Description]) -> Result<()> {
        let kind = Kind::Pocsag.as_str();
        let clear_sql = self.sql("DELETE FROM descriptions WHERE kind = ?");
        let delete_sql = self.sql("DELETE FROM descriptions WHERE kind = ? AND address = ?");
        run!(self, |pool| {
            let mut tx = pool.begin().await?;
            if clear {
                sqlx::query(&clear_sql).bind(kind).execute(&mut *tx).await?;
            }
            for address in delete {
                sqlx::query(&delete_sql).bind(kind).bind(*address).execute(&mut *tx).await?;
            }
            for chunk in insert.chunks(BATCH) {
                let mut query = QueryBuilder::new("INSERT INTO descriptions (kind, address, ort, kurz, lang) ");
                query.push_values(chunk, |mut row, d| {
                    row.push_bind(kind).push_bind(&d.address).push_bind(&d.ort).push_bind(&d.kurz).push_bind(&d.lang);
                });
                query.build().execute(&mut *tx).await?;
            }
            tx.commit().await
        })
    }
}

/// Wartet auf `f`, auch aus der Tokio-Laufzeit des Servers heraus (ohne deren Threads zu blockieren).
fn block_on<F: Future>(rt: &tokio::runtime::Runtime, f: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::block_in_place(|| rt.block_on(f)),
        Err(_) => rt.block_on(f),
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // Eine Laufzeit darf nicht innerhalb einer anderen beendet werden
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::Decoded;

    fn pocsag(address: &str, sub: &str) -> Telegram {
        let d = Decoded { kind: Kind::Pocsag, address: address.into(), sub: sub.into(), message: "Brand".into() };
        Telegram::new(d, "Test", chrono::Local::now())
    }

    #[test]
    fn insert_and_page() {
        let db = Db::open(":memory:").unwrap();
        for i in 0..6 {
            let d = Decoded { kind: Kind::Fms, address: format!("6263123{i}"), sub: "3".into(), message: "Einsatz übernommen".into() };
            let source = if i == 2 { "Andere" } else { "Test" };
            let id = db.insert(&Telegram::new(d, source, chrono::Local::now())).unwrap();
            assert_eq!(id, i + 1);
        }
        assert_eq!(db.sources().unwrap(), ["Andere", "Test"]);
        let page = db.latest("Test", 2, None).unwrap();
        assert_eq!(page.iter().map(|t| t.id).collect::<Vec<_>>(), [6, 5]);
        assert_eq!(page[0].kind, Kind::Fms);
        assert_eq!(page[0].message, "Einsatz übernommen");
        let older = db.latest("Test", 10, Some(5)).unwrap();
        assert_eq!(older.iter().map(|t| t.id).collect::<Vec<_>>(), [4, 2, 1]);
        assert_eq!(db.latest("Andere", 10, None).unwrap()[0].id, 3);
        assert!(db.contains(Kind::Fms, "62631230", "Einsatz übernommen").unwrap());
        assert!(!db.contains(Kind::Fms, "62631230", "anders").unwrap());
    }

    #[test]
    fn describe_prefers_exact_sub() {
        let db = Db::open(":memory:").unwrap();
        let d = |address: &str, ort: &str| Description { address: address.into(), ort: ort.into(), kurz: "FW".into(), lang: String::new() };
        db.write_descriptions(false, &[], &[d("1234567*", "alle"), d("1234567b", "nur b")]).unwrap();
        let mut a = pocsag("1234567", "a");
        db.describe(&mut a).unwrap();
        assert_eq!((a.ort.as_str(), a.kurz.as_str()), ("alle", "FW"));
        let mut b = pocsag("1234567", "b");
        db.describe(&mut b).unwrap();
        assert_eq!(b.ort, "nur b");
        let mut other = pocsag("7654321", "a");
        db.describe(&mut other).unwrap();
        assert_eq!(other.ort, "");

        db.insert(&b).unwrap();
        assert_eq!(db.latest("Test", 1, None).unwrap()[0].ort, "nur b");
        db.write_descriptions(false, &["1234567b"], &[d("1234567c", "c")]).unwrap();
        let mut all: Vec<_> = db.descriptions().unwrap().into_iter().map(|d| d.address).collect();
        all.sort();
        assert_eq!(all, ["1234567*", "1234567c"]);
        db.write_descriptions(true, &[], &[]).unwrap();
        assert!(db.descriptions().unwrap().is_empty());
    }

    /// Mit `BOSLOOK_TEST_DATABASES="mysql://… postgres://…"` auch gegen echte Server testen.
    #[test]
    fn other_databases() {
        for url in std::env::var("BOSLOOK_TEST_DATABASES").unwrap_or_default().split_whitespace() {
            let db = Db::open(url).unwrap();
            db.write_descriptions(true, &[], &[Description { address: "1234567*".into(), ort: "Großstadt".into(), kurz: "FW".into(), lang: "Löschzug".into() }]).unwrap();
            let mut t = pocsag("1234567", "a");
            db.describe(&mut t).unwrap();
            assert_eq!(t.ort, "Großstadt", "{url}");
            let id = db.insert(&t).unwrap();
            let latest = db.latest("Test", 1, None).unwrap();
            assert_eq!((latest[0].id, latest[0].lang.as_str()), (id, "Löschzug"), "{url}");
            assert!(db.sources().unwrap().contains(&"Test".to_string()));
            assert!(db.contains(Kind::Pocsag, "1234567", "Brand").unwrap());
        }
    }
}

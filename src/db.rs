use crate::telegram::{Kind, Telegram};
use anyhow::Result;
use rusqlite::{Connection, params};

pub struct Db(Connection);

impl Db {
    pub fn open(path: &str) -> Result<Db> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegrams (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                time TEXT NOT NULL,
                source TEXT NOT NULL,
                kind TEXT NOT NULL,
                address TEXT NOT NULL,
                sub TEXT NOT NULL,
                message TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS telegrams_source ON telegrams (source, id);",
        )?;
        Ok(Db(conn))
    }

    pub fn insert(&self, t: &Telegram) -> Result<i64> {
        self.0.execute(
            "INSERT INTO telegrams (time, source, kind, address, sub, message) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![t.time.to_rfc3339(), t.source, t.kind.as_str(), t.address, t.sub, t.message],
        )?;
        Ok(self.0.last_insert_rowid())
    }

    /// Alle Quellen, von denen Telegramme gespeichert sind.
    pub fn sources(&self) -> Result<Vec<String>> {
        let mut stmt = self.0.prepare("SELECT DISTINCT source FROM telegrams ORDER BY source")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Die neuesten Telegramme einer Quelle, optional nur die älteren als `before` (für Blättern).
    pub fn latest(&self, source: &str, limit: u32, before: Option<i64>) -> Result<Vec<Telegram>> {
        let mut stmt = self.0.prepare(
            "SELECT id, time, source, kind, address, sub, message FROM telegrams
             WHERE source = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![source, before.unwrap_or(i64::MAX), limit], |r| {
            let time: String = r.get(1)?;
            let kind: String = r.get(3)?;
            Ok(Telegram {
                id: r.get(0)?,
                time: chrono::DateTime::parse_from_rfc3339(&time)
                    .map(|t| t.with_timezone(&chrono::Local))
                    .unwrap_or_default(),
                source: r.get(2)?,
                kind: Kind::parse(&kind).unwrap_or(Kind::Pocsag),
                address: r.get(4)?,
                sub: r.get(5)?,
                message: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::Decoded;

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
    }
}

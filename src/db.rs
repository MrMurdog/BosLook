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
            );",
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

    /// Die neuesten Telegramme, optional nur die älteren als `before` (für Blättern).
    pub fn latest(&self, limit: u32, before: Option<i64>) -> Result<Vec<Telegram>> {
        let mut stmt = self.0.prepare(
            "SELECT id, time, source, kind, address, sub, message FROM telegrams
             WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![before.unwrap_or(i64::MAX), limit], |r| {
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
        for i in 0..5 {
            let d = Decoded { kind: Kind::Fms, address: format!("6263123{i}"), sub: "3".into(), message: "Einsatz übernommen".into() };
            let id = db.insert(&Telegram::new(d, "Test", chrono::Local::now())).unwrap();
            assert_eq!(id, i + 1);
        }
        let page = db.latest(2, None).unwrap();
        assert_eq!(page.iter().map(|t| t.id).collect::<Vec<_>>(), [5, 4]);
        assert_eq!(page[0].kind, Kind::Fms);
        assert_eq!(page[0].message, "Einsatz übernommen");
        let older = db.latest(10, Some(4)).unwrap();
        assert_eq!(older.iter().map(|t| t.id).collect::<Vec<_>>(), [3, 2, 1]);
    }
}

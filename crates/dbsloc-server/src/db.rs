// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! SQLite job queue (`<data_root>/server/jobs.db`) via `rusqlite` (bundled).

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use crate::timeutil::now_unix_millis;

/// One persisted job row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub dataset: String,
    pub subject: String,
    pub options: String,
    pub phase: String,
    pub priority: i64,
    pub resume: bool,
    pub gated_stage: Option<String>,
    pub stage: Option<String>,
    pub stages_ok: u32,
    pub stages_failed: u32,
    pub stages_skipped: u32,
    pub message: Option<String>,
    pub client: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub resumed_by: Option<String>,
}

/// A new job to insert.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub id: String,
    pub dataset: String,
    pub subject: String,
    pub options: String,
    pub priority: i64,
    pub resume: bool,
    pub gated_stage: Option<String>,
    pub client: Option<String>,
}

/// Handle to the SQLite database; cloning shares the connection behind a mutex.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

const SELECT_COLS: &str = "id, dataset, subject, options, phase, priority, resume, gated_stage, \
     stage, stages_ok, stages_failed, stages_skipped, message, client, created_at, started_at, \
     finished_at, resumed_by";

/// The phases in which a subject has an active run.
const ACTIVE_PHASES: &str = "'queued','running','awaiting_qc'";

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: row.get(0)?,
        dataset: row.get(1)?,
        subject: row.get(2)?,
        options: row.get(3)?,
        phase: row.get(4)?,
        priority: row.get(5)?,
        resume: row.get::<_, i64>(6)? != 0,
        gated_stage: row.get(7)?,
        stage: row.get(8)?,
        stages_ok: row.get::<_, i64>(9)? as u32,
        stages_failed: row.get::<_, i64>(10)? as u32,
        stages_skipped: row.get::<_, i64>(11)? as u32,
        message: row.get(12)?,
        client: row.get(13)?,
        created_at: row.get(14)?,
        started_at: row.get(15)?,
        finished_at: row.get(16)?,
        resumed_by: row.get(17)?,
    })
}

fn insert_row(conn: &Connection, job: &NewJob, phase: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO jobs (id, dataset, subject, options, phase, priority, resume, gated_stage,
                           client, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            job.id,
            job.dataset,
            job.subject,
            job.options,
            phase,
            job.priority,
            i64::from(job.resume),
            job.gated_stage,
            job.client,
            now_unix_millis(),
        ],
    )?;
    Ok(())
}

/// Bring a database made by an earlier build up to date: the `resumed_by` column, one active run
/// per subject (older duplicates are failed) and the partial unique index that enforces it.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let mut has_resumed_by = false;
    {
        let mut stmt = conn.prepare("PRAGMA table_info(jobs)")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in names {
            if name? == "resumed_by" {
                has_resumed_by = true;
            }
        }
    }
    if !has_resumed_by {
        conn.execute("ALTER TABLE jobs ADD COLUMN resumed_by TEXT", [])?;
    }
    conn.execute(
        &format!(
            "UPDATE jobs SET phase='failed', finished_at=?1,
                 message=COALESCE(message, 'superseded duplicate active run')
             WHERE phase IN ({ACTIVE_PHASES})
               AND rowid NOT IN (
                   SELECT MAX(rowid) FROM jobs WHERE phase IN ({ACTIVE_PHASES})
                   GROUP BY dataset, subject
               )"
        ),
        params![now_unix_millis()],
    )?;
    conn.execute_batch(&format!(
        "CREATE UNIQUE INDEX IF NOT EXISTS jobs_one_active ON jobs(dataset, subject)
         WHERE phase IN ({ACTIVE_PHASES});"
    ))?;
    Ok(())
}

impl Db {
    /// Open (creating) the database at `path` and ensure the schema.
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS jobs (
                 id            TEXT PRIMARY KEY,
                 dataset       TEXT NOT NULL,
                 subject       TEXT NOT NULL,
                 options       TEXT NOT NULL,
                 phase         TEXT NOT NULL,
                 priority      INTEGER NOT NULL DEFAULT 1,
                 resume        INTEGER NOT NULL DEFAULT 0,
                 gated_stage   TEXT,
                 stage         TEXT,
                 stages_ok     INTEGER NOT NULL DEFAULT 0,
                 stages_failed INTEGER NOT NULL DEFAULT 0,
                 stages_skipped INTEGER NOT NULL DEFAULT 0,
                 message       TEXT,
                 client        TEXT,
                 created_at    INTEGER NOT NULL,
                 started_at    INTEGER,
                 finished_at   INTEGER,
                 resumed_by    TEXT
             );
             CREATE INDEX IF NOT EXISTS jobs_phase ON jobs(phase, priority, created_at);",
        )?;
        migrate(&conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// On server restart: `queued` jobs stay queued; `running` jobs become `failed`.
    pub fn reset_on_startup(&self) -> rusqlite::Result<usize> {
        let now = now_unix_millis();
        let n = self.lock().execute(
            "UPDATE jobs SET phase='failed', finished_at=?1,
                 message=COALESCE(message, 'server restarted while running')
             WHERE phase='running'",
            params![now],
        )?;
        Ok(n)
    }

    #[cfg(test)]
    pub fn insert(&self, job: &NewJob) -> rusqlite::Result<()> {
        insert_row(&self.lock(), job, "queued")
    }

    /// Insert a job directly in `phase` (restoring or importing a job; tests).
    pub fn insert_as(&self, job: &NewJob, phase: &str) -> rusqlite::Result<()> {
        insert_row(&self.lock(), job, phase)
    }

    /// Insert a `queued` job unless the subject already has an active run, in one immediate
    /// transaction; returns the blocking job when there is one.
    pub fn insert_unless_active(&self, job: &NewJob) -> rusqlite::Result<Option<JobRow>> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM jobs
                     WHERE dataset=?1 AND subject=?2 AND phase IN ({ACTIVE_PHASES})
                     ORDER BY created_at ASC LIMIT 1"
                ),
                params![job.dataset, job.subject],
                row_from,
            )
            .optional()?;
        if existing.is_some() {
            return Ok(existing);
        }
        insert_row(&tx, job, "queued")?;
        tx.commit()?;
        Ok(None)
    }

    /// Claim the next runnable job: oldest `queued` first (resume jobs have lower priority),
    /// skipping any subject that already has a `running` job. Marks the job `running`.
    pub fn take_next_queued(&self) -> rusqlite::Result<Option<JobRow>> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let job = tx
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM jobs j
                     WHERE phase='queued'
                       AND NOT EXISTS (
                           SELECT 1 FROM jobs r
                           WHERE r.phase='running'
                             AND r.dataset=j.dataset AND r.subject=j.subject
                       )
                     ORDER BY priority ASC, created_at ASC, id ASC
                     LIMIT 1"
                ),
                [],
                row_from,
            )
            .optional()?;
        if let Some(j) = &job {
            tx.execute(
                "UPDATE jobs SET phase='running', started_at=?1 WHERE id=?2",
                params![now_unix_millis(), j.id],
            )?;
        }
        tx.commit()?;
        Ok(job.map(|mut j| {
            j.phase = "running".to_owned();
            j.started_at = Some(now_unix_millis());
            j
        }))
    }

    /// Fetch one job.
    pub fn get(&self, id: &str) -> rusqlite::Result<Option<JobRow>> {
        self.lock()
            .query_row(
                &format!("SELECT {SELECT_COLS} FROM jobs WHERE id=?1"),
                params![id],
                row_from,
            )
            .optional()
    }

    /// A `queued`, `running` or `awaiting_qc` job for a subject, if any (the one-run-per-subject
    /// rule: a new full run must not start while an earlier one is waiting on QC).
    pub fn active_for_subject(
        &self,
        dataset: &str,
        subject: &str,
    ) -> rusqlite::Result<Option<JobRow>> {
        self.lock()
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM jobs
                     WHERE dataset=?1 AND subject=?2 AND phase IN ({ACTIVE_PHASES})
                     ORDER BY created_at ASC LIMIT 1"
                ),
                params![dataset, subject],
                row_from,
            )
            .optional()
    }

    /// Move a job from `from` to the terminal `to` in one conditional statement (a concurrent
    /// transition cannot be overwritten); returns whether a row changed.
    pub fn transition(
        &self,
        id: &str,
        from: &str,
        to: &str,
        message: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let n = self.lock().execute(
            "UPDATE jobs SET phase=?3, finished_at=?4, message=COALESCE(?5, message)
             WHERE id=?1 AND phase=?2",
            params![id, from, to, now_unix_millis(), message],
        )?;
        Ok(n > 0)
    }

    /// The most recent `awaiting_qc` job for a subject, if any.
    pub fn awaiting_qc_for_subject(
        &self,
        dataset: &str,
        subject: &str,
    ) -> rusqlite::Result<Option<JobRow>> {
        self.lock()
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM jobs
                     WHERE dataset=?1 AND subject=?2 AND phase='awaiting_qc'
                     ORDER BY created_at DESC LIMIT 1"
                ),
                params![dataset, subject],
                row_from,
            )
            .optional()
    }

    /// Update the live progress counters and current stage.
    pub fn update_progress(
        &self,
        id: &str,
        stage: Option<&str>,
        ok: u32,
        failed: u32,
        skipped: u32,
        message: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE jobs SET stage=?2, stages_ok=?3, stages_failed=?4, stages_skipped=?5,
                             message=COALESCE(?6, message)
             WHERE id=?1",
            params![id, stage, ok, failed, skipped, message],
        )?;
        Ok(())
    }

    /// Worker completion: only a `running` job transitions, so a concurrent transition wins.
    pub fn finish(
        &self,
        id: &str,
        phase: &str,
        ok: u32,
        failed: u32,
        skipped: u32,
        message: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let n = self.lock().execute(
            "UPDATE jobs SET phase=?2, stages_ok=?3, stages_failed=?4, stages_skipped=?5,
                             message=COALESCE(?6, message), finished_at=?7
             WHERE id=?1 AND phase='running'",
            params![id, phase, ok, failed, skipped, message, now_unix_millis()],
        )?;
        Ok(n > 0)
    }

    /// Retire an `awaiting_qc` job after its gate was approved: claim it (`resumed` when a resume
    /// job follows, `done` otherwise) and insert the resume job in one transaction. Returns `false`
    /// when another request already claimed the job.
    pub fn retire_awaiting(&self, id: &str, resume: Option<&NewJob>) -> rusqlite::Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let phase = if resume.is_some() { "resumed" } else { "done" };
        let n = tx.execute(
            "UPDATE jobs SET phase=?2, finished_at=?3, resumed_by=?4, message=NULL
             WHERE id=?1 AND phase='awaiting_qc'",
            params![id, phase, now_unix_millis(), resume.map(|j| j.id.as_str())],
        )?;
        if n == 0 {
            return Ok(false);
        }
        if let Some(job) = resume {
            insert_row(&tx, job, "queued")?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Record the stage whose QC gate stopped the run, for a later resume.
    pub fn set_gated_stage(&self, id: &str, stage: Option<&str>) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE jobs SET gated_stage=?2 WHERE id=?1",
            params![id, stage],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Db) {
        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("server/jobs.db")).unwrap();
        (d, db)
    }

    fn new_job(id: &str, subject: &str, priority: i64, resume: bool) -> NewJob {
        NewJob {
            id: id.to_owned(),
            dataset: "demo".to_owned(),
            subject: subject.to_owned(),
            options: "{}".to_owned(),
            priority,
            resume,
            gated_stage: None,
            client: Some("test".to_owned()),
        }
    }

    #[test]
    fn insert_and_get_round_trip() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        let got = db.get("a").unwrap().unwrap();
        assert_eq!(got.id, "a");
        assert_eq!(got.phase, "queued");
        assert_eq!(got.dataset, "demo");
        assert_eq!(got.subject, "01");
        assert!(!got.resume);
        assert!(got.started_at.is_none());
        assert!(got.resumed_by.is_none());
    }

    #[test]
    fn take_next_marks_running_and_orders_resume_first() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        db.insert(&new_job("b", "02", 0, true)).unwrap();
        let first = db.take_next_queued().unwrap().unwrap();
        assert_eq!(first.id, "b");
        assert_eq!(first.phase, "running");
        let second = db.take_next_queued().unwrap().unwrap();
        assert_eq!(second.id, "a");
        assert!(db.take_next_queued().unwrap().is_none());
    }

    #[test]
    fn a_second_active_job_for_a_subject_is_refused_by_the_index() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        assert!(db.insert(&new_job("b", "01", 1, false)).is_err());
        db.insert(&new_job("c", "02", 1, false)).unwrap();
        let first = db.take_next_queued().unwrap().unwrap();
        assert_eq!(first.id, "a");
        assert!(db.insert(&new_job("d", "01", 1, false)).is_err());
        assert!(db.finish("a", "done", 1, 0, 0, None).unwrap());
        db.insert(&new_job("e", "01", 1, false)).unwrap();
    }

    #[test]
    fn insert_unless_active_reports_the_blocking_job() {
        let (_d, db) = db();
        assert!(db
            .insert_unless_active(&new_job("a", "01", 1, false))
            .unwrap()
            .is_none());
        let blocker = db
            .insert_unless_active(&new_job("b", "01", 1, false))
            .unwrap()
            .unwrap();
        assert_eq!(blocker.id, "a");
        assert!(db.get("b").unwrap().is_none());
    }

    #[test]
    fn awaiting_qc_lookup_and_finish_set_timestamps() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        db.take_next_queued().unwrap();
        assert!(db.finish("a", "awaiting_qc", 1, 0, 0, Some("qc")).unwrap());
        let awaiting = db.awaiting_qc_for_subject("demo", "01").unwrap().unwrap();
        assert_eq!(awaiting.id, "a");
        assert!(awaiting.finished_at.is_some());
        assert_eq!(awaiting.message.as_deref(), Some("qc"));
    }

    #[test]
    fn finish_only_transitions_a_running_job() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        assert!(!db.finish("a", "done", 1, 0, 0, None).unwrap());
        db.take_next_queued().unwrap();
        assert!(db.transition("a", "running", "cancelled", None).unwrap());
        assert!(!db.finish("a", "done", 1, 0, 0, None).unwrap());
        assert_eq!(db.get("a").unwrap().unwrap().phase, "cancelled");
    }

    #[test]
    fn startup_marks_running_as_failed() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        db.take_next_queued().unwrap();
        let n = db.reset_on_startup().unwrap();
        assert_eq!(n, 1);
        let got = db.get("a").unwrap().unwrap();
        assert_eq!(got.phase, "failed");
        assert!(got.message.unwrap().contains("restarted"));
    }

    #[test]
    fn active_lookup_matches_queued_running_and_awaiting_qc() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        assert!(db.active_for_subject("demo", "01").unwrap().is_some());
        db.take_next_queued().unwrap();
        db.finish("a", "awaiting_qc", 1, 0, 0, None).unwrap();
        assert!(
            db.active_for_subject("demo", "01").unwrap().is_some(),
            "an awaiting_qc run blocks a new full run"
        );
        assert!(db.retire_awaiting("a", None).unwrap());
        assert!(db.active_for_subject("demo", "01").unwrap().is_none());
    }

    #[test]
    fn transition_moves_only_from_the_expected_phase() {
        let (_d, db) = db();
        db.insert(&new_job("a", "01", 1, false)).unwrap();
        assert!(db
            .transition("a", "queued", "cancelled", Some("cancelled before start"))
            .unwrap());
        assert_eq!(db.get("a").unwrap().unwrap().phase, "cancelled");
        assert!(!db.transition("a", "queued", "cancelled", None).unwrap());
    }

    #[test]
    fn retire_awaiting_claims_once_and_inserts_one_resume() {
        let (_d, db) = db();
        db.insert_as(&new_job("a", "01", 1, false), "awaiting_qc")
            .unwrap();
        let resume = new_job("r", "01", 0, true);
        assert!(db.retire_awaiting("a", Some(&resume)).unwrap());
        assert!(!db
            .retire_awaiting("a", Some(&new_job("r2", "01", 0, true)))
            .unwrap());
        let a = db.get("a").unwrap().unwrap();
        assert_eq!(a.phase, "resumed");
        assert_eq!(a.resumed_by.as_deref(), Some("r"));
        assert_eq!(db.get("r").unwrap().unwrap().phase, "queued");
        assert!(db.get("r2").unwrap().is_none());
    }

    #[test]
    fn retire_without_a_resume_job_is_done() {
        let (_d, db) = db();
        db.insert_as(&new_job("a", "01", 1, false), "awaiting_qc")
            .unwrap();
        assert!(db.retire_awaiting("a", None).unwrap());
        let a = db.get("a").unwrap().unwrap();
        assert_eq!(a.phase, "done");
        assert!(a.resumed_by.is_none());
    }

    #[test]
    fn an_older_database_without_the_new_column_migrates() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("jobs.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE jobs (
                     id TEXT PRIMARY KEY, dataset TEXT NOT NULL, subject TEXT NOT NULL,
                     options TEXT NOT NULL, phase TEXT NOT NULL,
                     priority INTEGER NOT NULL DEFAULT 1, resume INTEGER NOT NULL DEFAULT 0,
                     gated_stage TEXT, stage TEXT, stages_ok INTEGER NOT NULL DEFAULT 0,
                     stages_failed INTEGER NOT NULL DEFAULT 0,
                     stages_skipped INTEGER NOT NULL DEFAULT 0, message TEXT, client TEXT,
                     created_at INTEGER NOT NULL, started_at INTEGER, finished_at INTEGER);
                 INSERT INTO jobs (id, dataset, subject, options, phase, created_at)
                     VALUES ('x', 'demo', '01', '{}', 'queued', 1),
                            ('y', 'demo', '01', '{}', 'awaiting_qc', 2);",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.get("x").unwrap().unwrap().phase, "failed");
        assert_eq!(db.get("y").unwrap().unwrap().phase, "awaiting_qc");
        assert!(db.insert(&new_job("z", "01", 1, false)).is_err());
    }
}

-- Durable "when did each named background task last run" state for the in-process
-- scheduler (see sure-scheduler / sure_dal::scheduled_tasks). This is unrelated to
-- `crons`/`cron_runs` above, which is a user-facing recurring-adjustment ledger — this
-- table backs generic, developer-defined background jobs (e.g. the exchange-rate poll)
-- so a process restart doesn't redo work ahead of schedule.
CREATE TABLE scheduled_task_runs (
    task_name    TEXT PRIMARY KEY,
    last_run_at  TEXT NOT NULL,
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

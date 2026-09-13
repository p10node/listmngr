-- Only the owner forwarding producer inserts these job-bound capabilities.
CREATE TABLE owner_deliveries (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES queue_jobs(id) ON DELETE CASCADE
);

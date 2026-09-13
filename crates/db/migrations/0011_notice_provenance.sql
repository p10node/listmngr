-- Producer-owned provenance survives workflow expiry and is not inferred from
-- untrusted message context/headers. Old unproven spools fail closed.
CREATE TABLE workflow_notices (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES queue_jobs(id) ON DELETE CASCADE
);
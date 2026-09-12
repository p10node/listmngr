-- Site-generated secrets (one-click unsubscribe MAC key). Generated once on
-- first use, never logged; delete a row to rotate.
CREATE TABLE site_secrets (
  name TEXT PRIMARY KEY,
  secret TEXT NOT NULL,
  created_at TEXT NOT NULL
);

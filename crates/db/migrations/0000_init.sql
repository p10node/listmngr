CREATE TABLE domains (
  id TEXT PRIMARY KEY,
  mail_host TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '',
  alias_domain TEXT,
  created_at TEXT NOT NULL
);
CREATE TABLE preferences (
  id TEXT PRIMARY KEY,
  acknowledge_posts INTEGER,
  hide_address INTEGER,
  preferred_language TEXT,
  receive_list_copy INTEGER,
  receive_own_postings INTEGER,
  delivery_mode TEXT,
  delivery_status TEXT
);
CREATE TABLE users (
  id TEXT PRIMARY KEY,
  display_name TEXT NOT NULL,
  is_server_owner INTEGER NOT NULL DEFAULT 0,
  preferences_id TEXT REFERENCES preferences(id) ON DELETE RESTRICT,
  locale TEXT NOT NULL DEFAULT 'en',
  timezone TEXT NOT NULL DEFAULT 'UTC',
  preferred_address_id TEXT,
  created_at TEXT NOT NULL
);
CREATE TABLE domain_owners (
  domain_id TEXT NOT NULL REFERENCES domains(id) ON DELETE RESTRICT,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  PRIMARY KEY(domain_id,user_id)
);
CREATE TABLE user_credentials (
  user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE RESTRICT,
  password_hash TEXT NOT NULL,
  password_updated_at TEXT NOT NULL,
  failed_attempts INTEGER NOT NULL DEFAULT 0,
  locked_until TEXT
);
CREATE TABLE addresses (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  original_email TEXT NOT NULL,
  display_name TEXT NOT NULL DEFAULT '',
  user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
  preferences_id TEXT REFERENCES preferences(id) ON DELETE RESTRICT,
  verified_on TEXT,
  registered_on TEXT NOT NULL
);
CREATE TABLE mailing_lists (
  list_id TEXT PRIMARY KEY,
  list_name TEXT NOT NULL,
  mail_host TEXT NOT NULL REFERENCES domains(mail_host) ON DELETE RESTRICT,
  display_name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  info TEXT NOT NULL DEFAULT '',
  subject_prefix TEXT NOT NULL,
  advertised INTEGER NOT NULL DEFAULT 1,
  preferred_language TEXT NOT NULL DEFAULT 'en',
  anonymous_list INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  last_post_at TEXT,
  post_id BIGINT NOT NULL DEFAULT 1,
  volume INTEGER NOT NULL DEFAULT 1,
  archive_policy TEXT NOT NULL DEFAULT 'public',
  archive_rendering_mode TEXT NOT NULL DEFAULT 'text',
  style_name TEXT NOT NULL DEFAULT 'legacy-default',
  extra TEXT NOT NULL DEFAULT '{}',
  UNIQUE(list_name,mail_host)
);
CREATE TABLE members (
  id TEXT PRIMARY KEY,
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE RESTRICT,
  role TEXT NOT NULL,
  address_id TEXT NOT NULL REFERENCES addresses(id) ON DELETE RESTRICT,
  user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
  subscription_mode TEXT NOT NULL,
  moderation_action TEXT,
  display_name TEXT NOT NULL DEFAULT '',
  preferences_id TEXT NOT NULL REFERENCES preferences(id) ON DELETE RESTRICT,
  bounce_score DOUBLE PRECISION NOT NULL DEFAULT 0,
  last_bounce_received TEXT,
  last_warning_sent TEXT,
  total_warnings_sent INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  UNIQUE(list_id,role,address_id)
);
CREATE TABLE api_tokens (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  scopes TEXT NOT NULL,
  list_id TEXT REFERENCES mailing_lists(list_id) ON DELETE RESTRICT,
  domain_id TEXT REFERENCES domains(id) ON DELETE RESTRICT,
  expires_at TEXT,
  last_used_at TEXT,
  revoked_at TEXT,
  created_at TEXT NOT NULL
);
CREATE TABLE audit_log (
  id TEXT PRIMARY KEY,
  at TEXT NOT NULL,
  actor_user_id TEXT,
  actor_token_id TEXT,
  ip TEXT,
  action TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id TEXT NOT NULL,
  diff TEXT NOT NULL
);
CREATE TABLE header_matches (
  id TEXT PRIMARY KEY,
  list_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  header TEXT NOT NULL,
  pattern TEXT NOT NULL,
  action TEXT,
  tag TEXT,
  chain TEXT
);
CREATE TABLE bans (id TEXT PRIMARY KEY, list_id TEXT, email_or_regex TEXT NOT NULL);
CREATE TABLE templates (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  scope TEXT NOT NULL,
  scope_id TEXT,
  language TEXT NOT NULL,
  uri TEXT,
  body TEXT,
  username TEXT,
  password TEXT,
  UNIQUE(name,scope,scope_id,language)
);
CREATE TABLE list_styles (name TEXT PRIMARY KEY, definition TEXT NOT NULL);
CREATE TABLE list_archivers (
  list_id TEXT NOT NULL REFERENCES mailing_lists(list_id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL,
  PRIMARY KEY(list_id,name)
);
INSERT INTO list_styles(name,definition) VALUES
 ('legacy-default','{"advertised":true,"archive_policy":"public"}'),
 ('legacy-announce','{"advertised":true,"archive_policy":"public"}'),
 ('private-default','{"advertised":false,"archive_policy":"private"}');

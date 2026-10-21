CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS approvals (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS task_dependencies (
    task_id TEXT NOT NULL,
    depends_on TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (task_id, depends_on)
);

CREATE TABLE IF NOT EXISTS credentials (
    reference TEXT PRIMARY KEY,
    secret TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS team_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS composer_history (
    id INTEGER PRIMARY KEY,
    workspace TEXT NOT NULL,
    prompt TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS composer_history_workspace_id
    ON composer_history(workspace, id);

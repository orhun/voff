CREATE TABLE IF NOT EXISTS task_dependencies (
    task_id TEXT NOT NULL,
    depends_on TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (task_id, depends_on)
);

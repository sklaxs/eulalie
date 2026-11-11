-- Eulalie : glossaire par projet + notes de traduction + timeout des traductions
CREATE TABLE IF NOT EXISTS glossary (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    term TEXT NOT NULL,
    translation TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(project_id, term)
);
CREATE INDEX IF NOT EXISTS idx_glossary_project ON glossary(project_id);

ALTER TABLE projects ADD COLUMN notes TEXT NOT NULL DEFAULT '';

ALTER TABLE user_settings ADD COLUMN translate_timeout INTEGER NOT NULL DEFAULT 10;

-- Eulalie : notes structurées par projet (liables à un chapitre) + côté du volet notes
CREATE TABLE IF NOT EXISTS project_notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    chapter_idx INTEGER,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_notes_project ON project_notes(project_id);

ALTER TABLE user_settings ADD COLUMN notes_side TEXT NOT NULL DEFAULT 'right';

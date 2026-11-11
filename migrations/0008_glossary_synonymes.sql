-- Eulalie : glossaire — autoriser les doublons de termes (synonymes).
-- SQLite ne permet pas de retirer une contrainte UNIQUE : la table est
-- reconstruite sans, les données existantes sont conservées.
CREATE TABLE glossary_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    term TEXT NOT NULL,
    translation TEXT NOT NULL,
    mistranslation TEXT,
    auto INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT INTO glossary_new (project_id, term, translation, mistranslation, auto, created_at)
    SELECT project_id, term, translation, mistranslation, auto, created_at FROM glossary;
DROP TABLE glossary;
ALTER TABLE glossary_new RENAME TO glossary;
CREATE INDEX IF NOT EXISTS idx_glossary_project ON glossary(project_id);

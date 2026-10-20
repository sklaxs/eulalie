-- Eulalie : langues de traduction par projet
ALTER TABLE projects ADD COLUMN source_lang TEXT NOT NULL DEFAULT 'en';
ALTER TABLE projects ADD COLUMN target_lang TEXT NOT NULL DEFAULT 'fr';

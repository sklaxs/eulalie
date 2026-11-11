-- Eulalie : application sélective du glossaire + fermeture du volet notes au clic extérieur
ALTER TABLE glossary ADD COLUMN auto INTEGER NOT NULL DEFAULT 1;
ALTER TABLE user_settings ADD COLUMN notes_close_outside INTEGER NOT NULL DEFAULT 1;

-- The icon a Project is shown with: a glyph the app knows by name, and the
-- colour it is drawn in. NULL in either is "the app's default for this
-- Project", so an upgrade needs nothing written.

ALTER TABLE projects ADD COLUMN icon TEXT;
ALTER TABLE projects ADD COLUMN icon_color TEXT;

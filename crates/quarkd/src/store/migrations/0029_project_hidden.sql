-- Whether a Project is hidden from the app's lists. A view preference: its
-- coordinator and workers keep running, unlike an archived Project's.

ALTER TABLE projects ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;

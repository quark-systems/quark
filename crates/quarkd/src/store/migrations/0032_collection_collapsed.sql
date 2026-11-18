-- Whether a collection is folded shut in the left list. A view detail kept
-- with the collection, so every client this daemon serves opens it the same.

ALTER TABLE collections ADD COLUMN collapsed INTEGER NOT NULL DEFAULT 0;

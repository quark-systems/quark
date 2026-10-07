-- Project creation: lifecycle status and the creation inputs (`spec`, JSON
-- with repos, agent config, dispatch preset and delivery policy).

ALTER TABLE projects ADD COLUMN status TEXT NOT NULL DEFAULT 'ready';
ALTER TABLE projects ADD COLUMN status_detail TEXT;
ALTER TABLE projects ADD COLUMN spec TEXT;
ALTER TABLE projects ADD COLUMN project_repo_path TEXT;

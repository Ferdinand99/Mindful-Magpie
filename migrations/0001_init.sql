-- Magpie's own migration set starts here (the kit's core set creates users,
-- sessions and tokens). This table keeps small facts about the instance and
-- makes sure the app's own migrations run from day one.
CREATE TABLE magpie_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

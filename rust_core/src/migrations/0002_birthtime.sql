-- The column called `ctime` never held ctime.
--
-- It is populated from Rust's `Metadata::created()`, which on macOS maps to
-- st_birthtime (when the file was created), NOT st_ctime (when the inode was
-- last changed). The name invited exactly the wrong reasoning, and every read
-- path collapsed it with mtime via MAX() anyway, so nothing ever noticed.
--
-- Renaming it is the precondition for letting the user pick which date drives
-- sorting: "Date Added" has to mean something specific to be forceable.
ALTER TABLE files RENAME COLUMN ctime TO birthtime;

-- Sorting by date added is now a first-class query, so it gets its own index
-- rather than falling back to a full scan the way MAX(mtime, ctime) forced.
CREATE INDEX files_birthtime_desc ON files(birthtime DESC);

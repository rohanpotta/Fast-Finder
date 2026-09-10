-- True "Date Added", pulled from Spotlight's kMDItemDateAdded.
--
-- `birthtime` answers "when was this file created", which is NOT what Finder's
-- Date Added column shows: Finder reports when the file arrived in its current
-- folder. For anything created a year ago and moved here yesterday the two
-- disagree completely, and birthtime is the wrong answer to the question the
-- user is actually asking.
--
-- Nullable on purpose, and the column doubles as the "has this file been
-- through the Spotlight puller yet" "flag: NULL means never pulled, 0 means
-- pulled but Spotlight had nothing. That keeps the puller incremental without
-- a second bookkeeping table.
ALTER TABLE files ADD COLUMN date_added INTEGER;

CREATE INDEX files_date_added_desc ON files(date_added DESC);

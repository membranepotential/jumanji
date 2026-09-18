# The build cache

For every output file the cache records a hash of each input that produced it.
A page is rebuilt only when one of those hashes changes.

The cache lives in [storage](storage.md); the rules for what counts as a
change are in [[incremental-builds]].

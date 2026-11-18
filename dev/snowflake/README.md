# Snowflake test fixtures

`fixtures/` holds responses recorded from a real account, replayed by the mock
in `src/db/snowflake/mock.rs`.

**No private key lives here.** The two `test-key-*.rsa.pub.der` files are the
public halves of throwaway key pairs, and only the fingerprint test needs them:
it checks our `SHA256:` against the value openssl computed from them, so the
oracle outlives the key that produced it and nothing secret has to stay in the
tree. The tests that have to *sign* draw a 2048-bit key the first time they run
and keep it in `target/test-keys/` (`src/db/snowflake/keys.rs`, which is
compiled only for tests). The two encrypted names those tests feed the reader
are headers with a placeholder under them: `key_pair` refuses an encrypted key
by its header before it decodes any of it, so there is nothing under them to
protect.

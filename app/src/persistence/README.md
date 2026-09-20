# Saving and restoring local data

On macOS, Galaxy stores its local database at `~/.galaxy/galaxy.sqlite`.
`general.restore_session` controls automatic reopening of the previous session;
conversation history and session snapshots continue to be saved when it is off.
Terminal scrollback retention does not limit persisted provider conversation context.

New conversation records store the provider message history and tool-result archive
alongside the progressive summary. Older records fall back to reconstructing the
available task messages; provider details omitted by those older saves cannot be
recovered exactly.

Startup reads use a consistent SQLite snapshot and recover independent data domains.
If a domain or saved tab/conversation cannot be read, Galaxy logs the failure, shows a persistent startup warning, and
leaves database writing disabled for that launch so partial in-memory state cannot
overwrite the original records. Repair the underlying problem and restart to resume
saving. Snapshot deduplication happens only after a successful database commit;
transactional snapshot and conversation saves retry transient database contention.

# How to perform migrations

## How do migrations work?
A sqlite database is one single file on the user's computer, and instead of being a separate process, it's a set of C functions
that are bundled in our app. When the warp app starts, we upgrade the schema to the latest version in a transaction.
Since we don't control the machine, we need to be super careful about the migrations that we ship.

TODO: C.I. and remediation of failed migrations

## Step 1: One-time setup
Make sure you have run `/script/bootstrap` at least once before. That installs our fork of
`diesel_cli`. Our fork is an old version that [bundles](https://github.com/warpdotdev/diesel/blob/b2c58897c39c519a946314bd5b63765d3af56204/diesel_cli/Cargo.toml#L54)
SQLite in with the `diesel_cli`. We use these version of Diesel and SQLite instead of relying on
the versions on our machines. So do not follow the official Diesel CLI installation instructions.

Additionally, the `sqlite3` binary is useful in development. The one on your Macbook is ok, but
likely will be missing some basic language features. You can grab the latest binary from the
official website https://www.sqlite.org/download.html. Note that we use SQL language features that
are not available on old versions of sqlite. Note that you'll have to approve it to be run in your mac system preferences.

## Step 2: Write the migration
```
diesel migration generate <descriptive name of your migration>
```
This will create a new folder with an up.sql and down.sql.

## Step 3: Run the migration + generate the schema
```
cd <repo root>
diesel migration run --database-url="/Users/$USER/.galaxy/galaxy.sqlite"
```
This will run the migration on the same warp that runs when you run the app locally. This automatically generates or updates the `crates/persistence/src/schema.rs`. We do not make manual edits to `schema.rs`.

You can also print the schema from a database that already has the migration with:
```
diesel print-schema --database-url="/Users/$USER/.galaxy/galaxy.sqlite"
```

## Reverting/redo-ing migrations
As you are writing features and changing branches, you'll want to undo migrations to fix your database and make it compatible with older code. Redo-ing can also be helpful as you are iterating on your schema.
```
diesel migration revert --database-url="/Users/$USER/.galaxy/galaxy.sqlite"
diesel migration redo --database-url="/Users/$USER/.galaxy/galaxy.sqlite"
```

# Schema style
- We use `id` for integer primary keys. If there's something more special about the primary key, consider a more descriptive name.
- Plural table names, singular for structs in the rust model code.
- If there's a table `foos` and a table `bars`, and `bars` has a column with a foreign key that references `foos.id`, name it `foo_id`.

# The `schema.patch` file
The `crates/persistence/schema.patch` is manually updated by us. This file lets us make manual
changes on top of the auto-generated `schema.rs` file produced by `diesel_cli`.

To create the `schema.patch` file, we:
1. Run the diesel migrations
1. Manually edit `schema.rs`
1. Run `git diff -U6 > crates/persistence/schema.patch`.

You can read more about this patch file in the official [Diesel documentation](https://diesel.rs/guides/configuring-diesel-cli.html#the-patch_file-field).

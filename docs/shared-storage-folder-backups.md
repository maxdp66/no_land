# Folder saves and bounded backup staging

The previous worker copied every selected file into a snapshot fallback, wrote all
new chunk payloads into its local CAS, and built the entire encrypted pack set
before starting cloud uploads. Large selections could consume roughly three
additional copies of their content and run out of VM disk before uploading.
Failed uploads retained packs even though a retry generated fresh random pack IDs.
An unrecognized export path also fell back to saving all discovered applications.
These are findings from the code; live provider credentials and VM logs were not
available in this workspace.

Backups now read files through a bounded chunk channel, create packs with a
64 MiB regular target and a 16 MiB small-file target, upload each pack using the
existing transfer journal, and remove it immediately. Only one encrypted pack is
staged on disk at a time. Failures and cancellations remove generated staging;
startup clears obsolete backup packs and snapshots and limits the legacy chunk
cache to 512 MiB, respecting pins. Restore transaction workspaces remain available
for rollback. The manifest, index, COMMITTED marker, and catalog are published
after all packs have uploaded successfully.

Live reads are labeled best effort. If a file's identity, size, or modification
time changes during its read, the save fails with an instruction to close its
application and retry. This avoids the full-selection snapshot copy; it does not
promise an application-consistent snapshot of files changing between reads.

In Export To Shared Storage, enter a VM folder path and choose Add Folder, then
Export Selected. Paths can be relative to the VM user's home or absolute within
that home. Folder snapshots include nested regular files, empty directories,
and supported relative symlinks without following them. External links, parent
traversal links, special files, the entire home, and worker storage roots are
rejected. Folder paths are explicit selections and never imply all applications.

To restore, open Sync and select a bundle under the saved Folder entry. Its
contents restore at the same home-relative path on the destination VM, using the
existing verified download and rollback transaction. Existing unrelated files
are retained. Each folder save captures its current tree independently, so files
removed from the source do not reappear in later snapshots. A restored folder can
be saved again. Agent API version 20 makes the desktop refresh older VM agents.

Regression coverage includes a folder round trip to a fresh home, empty
folders and symlinks, path rejection, upload starting before all files have been
processed, upload failure and retry cleanup, and selection parsing.

## Restore completion verification

A restore checks authenticated packs and chunk hashes on download, then checks
reconstructed file sizes and BLAKE3 hashes before atomic publication. Before
committing, it now rechecks every selected final destination: regular-file type,
size, BLAKE3 hash and saved permission bits; empty-directory existence/type; and
symlink existence/type and exact target. Every selected entry, including those
without chunks, must have been published. Applicable tombstones must have been
applied and their deleted file destinations remain absent.

The worker reports a verifying phase and persists destination verification counts
(files, directories, symlinks and bytes) with successful operation progress.
Verification runs while rollback data is retained, so a failure prevents the
completion notification and reverts changes owned by the restore. Keep writing
applications closed during restore; verification describes the destinations at
commit time and cannot prevent later changes by another process.

Regression tests cover same-size corruption, wrong-length chunks, unpublished
empty entries, changed symlink targets, removed empty directories, executable
permissions, and a corrupted encrypted cloud pack followed by repair and retry.
Folder retries preserve unrelated existing files. File and folder round trips
assert their final verification counts.

//! Durable state for a computer.
//!
//! The guest can be rebuilt, replaced, or rolled back, and the user's work
//! has to survive all three. This crate owns that state independently of what
//! will run the guest (a container or a microVM) once one exists; today it is
//! an ordinary process.
//!
//! # Layout
//!
//! ```text
//! <root>/volumes/<id>/
//!   meta.json           sequence counter + snapshot index
//!   current/            live data; this is what the guest mounts as /workspace
//!   blobs/<ab>/<hash>   content-addressed file bodies, shared across snapshots
//!   manifests/<sid>.json  path -> hash for one point in time
//!   current.new/        only present mid-restore (see Crash safety)
//! ```
//!
//! # Snapshots are content-addressed
//!
//! Hard links are not used. A hard link is a second name for the same inode,
//! so a linked snapshot only preserves history if every writer replaces files
//! rather than modifying them. Editors that write-to-temp-and-rename do
//! replace; `fs::write`, a shell `>` redirect, an append, and most programs
//! writing a file they already opened do not. They truncate in place, and
//! every snapshot pointing at that inode silently changes with it. This
//! property is held by `a_snapshot_is_immutable_once_taken`.
//!
//! Instead, each file is hashed and its bytes are copied into a
//! content-addressed blob store keyed by that hash. Identical content is
//! stored once however many snapshots reference it, so the first snapshot pays
//! a full copy and later ones pay only for what changed. A blob cannot be
//! mutated through the live tree, because nothing in the live tree shares its
//! inode.
//!
//! # Crash safety
//!
//! Restore never mutates `current` in place. It builds `current.new`, then
//! swaps. If the process dies mid-swap, [`Volume::open`] finishes it on the
//! next start rather than leaving a half-restored workspace.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("no snapshot `{0}`")]
    NoSuchSnapshot(String),
    #[error("volume metadata is unreadable: {0}")]
    BadMeta(String),
    #[error("refusing to operate on a volume with no `current` directory; it may be mid-restore and unrecoverable")]
    NoCurrent,
    #[error("a guest is running on this volume; stop it first")]
    Attached,
    #[error("another operation is rewriting this volume's history; try again when it finishes")]
    Busy,
}

/// How long to wait for another snapshot, restore or prune to finish.
///
/// A snapshot of a large workspace is a full copy and can outlast this. When
/// it does, the caller gets [`StoreError::Busy`] rather than an indefinite
/// hang with no output.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Holds a volume against concurrent history rewrites. See
/// [`Volume::lock_mutations`].
struct MutationGuard(fs::File);

impl Drop for MutationGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotId(pub String);

impl SnapshotId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: SnapshotId,
    /// Why the snapshot was taken. Safety snapshots taken by [`Volume::restore`]
    /// say so, which is what makes an accidental reset recoverable from a
    /// listing.
    pub label: String,
    /// Ordering key. A counter rather than a clock, so snapshots order
    /// correctly across clock skew and tests do not depend on wall time.
    pub seq: u64,
    pub files: u64,
    pub bytes: u64,
}

/// One file inside a snapshot: where it lived and what it contained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    path: String,
    hash: String,
    len: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Meta {
    next_seq: u64,
    snapshots: BTreeMap<String, Snapshot>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("volumes"))?;
        Ok(Self { root })
    }

    /// Attach a volume, creating it if this is its first use.
    pub fn volume(&self, id: &str) -> Result<Volume> {
        let dir = self.root.join("volumes").join(sanitize(id));
        fs::create_dir_all(dir.join("blobs"))?;
        fs::create_dir_all(dir.join("manifests"))?;
        let v = Volume {
            id: id.to_owned(),
            dir,
            lock_wait: LOCK_WAIT,
        };
        v.recover_interrupted_restore()?;
        fs::create_dir_all(v.workspace())?;
        Ok(v)
    }

    pub fn volumes(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let dir = self.root.join("volumes");
        if !dir.exists() {
            return Ok(out);
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                out.push(e.file_name().to_string_lossy().into_owned());
            }
        }
        out.sort();
        Ok(out)
    }
}

pub struct Volume {
    id: String,
    dir: PathBuf,
    /// Injectable so the [`StoreError::Busy`] path is reachable in a test
    /// without waiting the full [`LOCK_WAIT`].
    lock_wait: std::time::Duration,
}

/// Holds a volume for the lifetime of a guest.
///
/// The claim is an OS lock on the file, not the file's existence, so it is
/// released both when this drops and when the process dies without dropping
/// anything.
pub struct Attached {
    file: fs::File,
}

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Hold a plain directory the way a volume is held.
///
/// A durable volume refuses a second guest. A `--workspace` directory needs
/// the same protection: without it, two `openbot up` instances on one directory
/// both start and both serve agents writing the same files. Choosing a plain
/// workspace opts out of snapshots, not out of exclusive access.
///
/// The lock file lives inside the directory it protects, because the path is
/// the only thing that identifies it: a second run pointed at the same path
/// finds the same file. It is visible to `fs.list` as a real file in the
/// workspace. Deleting it releases nothing, since the lock is held by the OS
/// against an open handle rather than by the file existing.
///
/// # Errors
/// [`StoreError::Attached`] if another process holds it, or io if the
/// directory cannot be written.
pub fn hold_dir(dir: &Path) -> Result<Attached> {
    fs::create_dir_all(dir)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".openbot.lock"))?;
    match f.try_lock() {
        Ok(()) => {
            use std::io::Write;
            let _ = f.set_len(0);
            let _ = write!(f, "{}", std::process::id());
            Ok(Attached { file: f })
        }
        Err(fs::TryLockError::WouldBlock) => Err(StoreError::Attached),
        Err(fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

impl Volume {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The live directory. This is what a guest mounts.
    pub fn workspace(&self) -> PathBuf {
        self.dir.join("current")
    }

    /// Durable home for the browser profile.
    ///
    /// Intentionally outside `current/`. A browser profile holds cookies and
    /// a `Login Data` database, the credentials for every site the user signed
    /// into. Inside the workspace it would be readable by `fs.read`, which is
    /// allow-listed and never prompts, turning a read-only tool into a
    /// credential exfiltration path. The profile is durable so sessions
    /// survive a rebuild, and it is not snapshotted: rolling a workspace back
    /// should not silently restore expired logins, and copying a live profile
    /// is unreliable.
    pub fn browser_profile(&self) -> PathBuf {
        self.dir.join("browser-profile")
    }

    fn staging(&self) -> PathBuf {
        self.dir.join("current.new")
    }
    fn retired(&self) -> PathBuf {
        self.dir.join("current.old")
    }
    fn meta_path(&self) -> PathBuf {
        self.dir.join("meta.json")
    }
    fn blob_path(&self, hash: &str) -> PathBuf {
        self.dir.join("blobs").join(&hash[..2]).join(hash)
    }
    fn manifest_path(&self, id: &SnapshotId) -> PathBuf {
        self.dir.join("manifests").join(format!("{}.json", id.0))
    }

    fn read_manifest(&self, id: &SnapshotId) -> Result<Manifest> {
        let s = fs::read_to_string(self.manifest_path(id))
            .map_err(|_| StoreError::NoSuchSnapshot(id.0.clone()))?;
        serde_json::from_str(&s).map_err(|e| StoreError::BadMeta(e.to_string()))
    }

    fn read_meta(&self) -> Result<Meta> {
        match fs::read_to_string(self.meta_path()) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| StoreError::BadMeta(e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Meta::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write metadata through a temp file and a rename, so a crash leaves
    /// either the old index or the new one, never a truncated file.
    fn write_meta(&self, m: &Meta) -> Result<()> {
        let tmp = self.dir.join("meta.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(m).expect("meta serialises"))?;
        fs::rename(&tmp, self.meta_path())?;
        Ok(())
    }

    /// Finish a restore that was interrupted between staging and swap.
    ///
    /// Without this, a crash in that window leaves a workspace that is neither
    /// the old state nor the new one.
    fn recover_interrupted_restore(&self) -> Result<()> {
        let (current, staging, retired) = (self.workspace(), self.staging(), self.retired());

        // Crashed after `current` moved aside but before staging took its
        // place: the new state is ready, so complete the move.
        if !current.exists() && staging.exists() {
            fs::rename(&staging, &current)?;
        }
        // Crashed before staging was complete: the old state is intact under
        // `current.old`, so put it back.
        if !current.exists() && retired.exists() {
            fs::rename(&retired, &current)?;
        }
        // Leftovers from a completed swap.
        if retired.exists() {
            fs::remove_dir_all(&retired)?;
        }
        if current.exists() && staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        Ok(())
    }

    /// Hold the volume against any other operation that rewrites its history.
    ///
    /// This closes two silent races:
    ///
    /// * [`Self::snapshot`] ingests every blob before writing the manifest
    ///   that names them, so a [`Self::prune`] in that window sees nothing
    ///   referencing those blobs and collects them. The snapshot then finishes
    ///   and is listed, pointing at bytes that are gone. Ingest is a full copy
    ///   of the workspace, so the window can be minutes long.
    /// * Two snapshots read `next_seq` before either writes it, take the same
    ///   id, and the second overwrites the first in `meta.json`: one snapshot
    ///   where there should be two, with no error reported.
    ///
    /// The lock is an OS advisory lock, not a lock file holding a pid. The
    /// kernel drops it when the handle closes, however the process ended, so a
    /// crash mid-snapshot cannot wedge the volume. A hand-rolled lock file
    /// would trade these races for a stale lock that has to be cleared by
    /// hand.
    fn lock_mutations(&self) -> Result<MutationGuard> {
        // The lock file is never removed: deleting it is itself a race, and an
        // empty file costs nothing.
        let f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.dir.join("mutate.lock"))?;

        // Bounded wait, so a contended volume produces `Busy` rather than an
        // indefinite hang.
        let deadline = std::time::Instant::now() + self.lock_wait;
        loop {
            match f.try_lock() {
                Ok(()) => return Ok(MutationGuard(f)),
                Err(fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(StoreError::Busy);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    }

    /// Take a point-in-time snapshot of the live workspace.
    pub fn snapshot(&self, label: &str) -> Result<Snapshot> {
        let _held = self.lock_mutations()?;
        self.snapshot_locked(label)
    }

    /// Take a snapshot with the mutation lock already held.
    ///
    /// Separate so [`Self::restore`] can take its safety snapshot inside the
    /// lock it already holds, rather than deadlocking against itself.
    fn snapshot_locked(&self, label: &str) -> Result<Snapshot> {
        if !self.workspace().exists() {
            return Err(StoreError::NoCurrent);
        }
        let mut meta = self.read_meta()?;
        let seq = meta.next_seq;
        meta.next_seq += 1;

        let id = SnapshotId(format!("snap-{seq:06}"));

        let mut manifest = Manifest::default();
        let ws = self.workspace();
        let mut n = 0u64;
        self.ingest(&ws, &ws, &mut manifest, &mut n)?;
        manifest.entries.sort_by(|a, b| a.path.cmp(&b.path));
        let bytes: u64 = manifest.entries.iter().map(|e| e.len).sum();

        let tmp = self.manifest_path(&id).with_extension("tmp");
        fs::write(
            &tmp,
            serde_json::to_vec(&manifest).expect("manifest serialises"),
        )?;
        fs::rename(&tmp, self.manifest_path(&id))?;

        let snap = Snapshot {
            id: id.clone(),
            label: label.to_owned(),
            seq,
            files: manifest.entries.len() as u64,
            bytes,
        };
        meta.snapshots.insert(id.0.clone(), snap.clone());
        self.write_meta(&meta)?;
        Ok(snap)
    }

    /// Walk `dir`, capturing every file into the blob store.
    ///
    /// The order is: copy first, hash the copy, then name it after that hash.
    /// Hashing the live file and copying it afterwards is not equivalent: the
    /// guest can write in between, and the blob would then hold the new bytes
    /// under the old content's name. Every later snapshot of that content
    /// hashes differently, finds no blob, and stores its own, so the
    /// mislabelled blob survives and every manifest referencing it restores
    /// the wrong bytes, silently and permanently. Hashing the stored copy
    /// guarantees the name always describes the contents. Held by
    /// `blobs_always_match_their_own_hash_under_concurrent_writes`.
    ///
    /// Symlinks are skipped. A link inside a snapshot could point outside the
    /// workspace, and following it on restore would write through the tenant
    /// boundary the guest enforces.
    fn ingest(&self, root: &Path, dir: &Path, out: &mut Manifest, n: &mut u64) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let path = entry.path();

            if ty.is_dir() {
                self.ingest(root, &path, out, n)?;
                continue;
            }
            if !ty.is_file() {
                continue;
            }

            let rel = path
                .strip_prefix(root)
                .map_err(|e| StoreError::BadMeta(e.to_string()))?
                .to_string_lossy()
                // Manifests are portable: a snapshot taken on Windows must
                // restore on Linux.
                .replace('\\', "/");

            *n += 1;
            let tmp = self.dir.join("blobs").join(format!("ingest-{n}.tmp"));
            match fs::copy(&path, &tmp) {
                Ok(_) => {}
                // The guest deleted it mid-walk. A snapshot is a point in
                // time, and this file was not in it; that is not a failure.
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            }

            let (hash, len) = hash_file(&tmp)?;
            let blob = self.blob_path(&hash);
            if blob.exists() {
                fs::remove_file(&tmp)?;
            } else {
                if let Some(parent) = blob.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(&tmp, &blob)?;
            }
            out.entries.push(Entry {
                path: rel,
                hash,
                len,
            });
        }
        Ok(())
    }

    /// Snapshots, oldest first.
    pub fn snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut v: Vec<_> = self.read_meta()?.snapshots.into_values().collect();
        v.sort_by_key(|s| s.seq);
        Ok(v)
    }

    pub fn latest(&self) -> Result<Option<Snapshot>> {
        Ok(self.snapshots()?.pop())
    }

    /// Path of the attach lock. Present while a guest holds the volume.
    fn lock_path(&self) -> PathBuf {
        self.dir.join("attached.lock")
    }

    /// Claim the volume for a running guest.
    ///
    /// Fails if another process already holds it. The lock is released when
    /// the returned guard drops, on a panic unwind, and when the process dies
    /// without unwinding at all.
    pub fn attach(&self) -> Result<Attached> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;

        match f.try_lock() {
            Ok(()) => {
                // The pid is informational, for someone reading the file. The
                // OS lock is what holds the volume.
                use std::io::Write;
                let _ = f.set_len(0);
                let _ = write!(f, "{}", std::process::id());
                Ok(Attached { file: f })
            }
            Err(fs::TryLockError::WouldBlock) => Err(StoreError::Attached),
            Err(fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// Whether a guest currently holds this volume.
    ///
    /// Probes the OS lock rather than checking for the file, so a lock file
    /// left behind by a dead process does not read as a running guest.
    pub fn is_attached(&self) -> bool {
        let Ok(f) = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path())
        else {
            return false;
        };
        match f.try_lock() {
            Ok(()) => {
                let _ = f.unlock();
                false
            }
            Err(_) => true,
        }
    }

    /// Remove a stale `attached.lock` file.
    ///
    /// Retained for compatibility with the documented `force-detach` command.
    /// Because the claim is an OS lock rather than the file's existence, a
    /// crashed guest releases the volume automatically and the next start
    /// works without this.
    ///
    /// Refuses when the volume is genuinely held: on Unix the open handle
    /// survives the unlink, so deleting the file would let a second guest
    /// take a fresh lock file and the same workspace.
    pub fn force_detach(&self) -> Result<()> {
        if self.is_attached() {
            return Err(StoreError::Attached);
        }
        match fs::remove_file(self.lock_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Roll the workspace back to a snapshot.
    ///
    /// Always takes a safety snapshot of the current state first and returns
    /// it, so a mistaken restore is undone by another restore. A rollback
    /// must never be able to discard recent work.
    pub fn restore(&self, id: &SnapshotId) -> Result<Snapshot> {
        let _held = self.lock_mutations()?;
        let meta = self.read_meta()?;
        if !meta.snapshots.contains_key(&id.0) {
            return Err(StoreError::NoSuchSnapshot(id.0.clone()));
        }
        let manifest = self.read_manifest(id)?;

        // Swapping the directory out from under a running guest fails
        // differently per platform: Windows refuses the rename while a handle
        // is open, and Linux allows it, leaving the guest writing into an
        // orphaned inode that restore then deletes. Refuse instead.
        if self.is_attached() {
            return Err(StoreError::Attached);
        }

        let safety = self.snapshot_locked(&format!("before restoring {id}"))?;

        let (current, staging, retired) = (self.workspace(), self.staging(), self.retired());
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        for e in &manifest.entries {
            let dest = staging.join(&e.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            // Copy rather than link, for the same reason ingest copies: a
            // link would let the next in-place write reach back into the blob.
            fs::copy(self.blob_path(&e.hash), &dest)?;
        }

        // Two renames rather than one: no filesystem can atomically replace a
        // non-empty directory, so the swap is staged such that every
        // interruption point is recoverable by `recover_interrupted_restore`.
        fs::rename(&current, &retired)?;
        fs::rename(&staging, &current)?;
        fs::remove_dir_all(&retired)?;

        Ok(safety)
    }

    /// When this snapshot's manifest was written, if the filesystem knows.
    ///
    /// Snapshots order by a counter rather than a clock, but "how long since
    /// the last one" is still useful: a schedule that has quietly stopped
    /// looks like one that is working until a rollback is needed. The
    /// manifest's mtime answers that without making a clock load-bearing.
    pub fn taken_at(&self, id: &SnapshotId) -> Option<std::time::SystemTime> {
        fs::metadata(self.manifest_path(id))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Delete all but the newest `keep` snapshots. Returns how many went.
    pub fn prune(&self, keep: usize) -> Result<usize> {
        let _held = self.lock_mutations()?;
        self.prune_where(keep, |_| true)
    }

    /// Keep the newest `keep` snapshots carrying this label, and delete older
    /// ones that carry it. Snapshots with other labels are left alone.
    ///
    /// This lets a schedule bound its own history without touching anything
    /// else. A timer trimming by total count would eventually delete a
    /// snapshot somebody took by hand before a risky change, silently.
    pub fn prune_labelled(&self, label: &str, keep: usize) -> Result<usize> {
        let _held = self.lock_mutations()?;
        self.prune_where(keep, |s| s.label == label)
    }

    /// Shared body of the prune operations. The caller holds the mutation
    /// lock.
    fn prune_where(&self, keep: usize, mine: impl Fn(&Snapshot) -> bool) -> Result<usize> {
        let mut meta = self.read_meta()?;
        let mut all: Vec<_> = meta
            .snapshots
            .values()
            .filter(|s| mine(s))
            .cloned()
            .collect();
        all.sort_by_key(|s| s.seq);
        if all.len() <= keep {
            return Ok(0);
        }
        let doomed = all.len() - keep;
        let mut removed = 0;
        for s in all.into_iter().take(doomed) {
            let m = self.manifest_path(&s.id);
            if m.exists() {
                fs::remove_file(&m)?;
            }
            meta.snapshots.remove(&s.id.0);
            removed += 1;
        }
        self.write_meta(&meta)?;
        self.gc_blobs(&meta)?;
        Ok(removed)
    }

    /// Delete blobs no surviving manifest references.
    ///
    /// Runs after the manifests are gone and metadata is written, so an
    /// interruption leaves unreferenced blobs (wasted space, recovered by the
    /// next prune) rather than a manifest pointing at a blob that is not
    /// there.
    ///
    /// The caller must hold the mutation lock. [`Volume::snapshot`] ingests
    /// every blob, then writes its manifest, then records itself in
    /// `meta.json`; a prune running in that window would see no manifest
    /// naming those blobs and collect them, and the snapshot would then
    /// complete and be listed pointing at bytes that are gone. Ingest is a
    /// full copy of the workspace, so the window is not small. Every caller of
    /// this function reaches it through [`Volume::lock_mutations`].
    fn gc_blobs(&self, meta: &Meta) -> Result<()> {
        let mut live = std::collections::BTreeSet::new();
        for id in meta.snapshots.keys() {
            if let Ok(m) = self.read_manifest(&SnapshotId(id.clone())) {
                for e in m.entries {
                    live.insert(e.hash);
                }
            }
        }
        let blobs = self.dir.join("blobs");
        if !blobs.exists() {
            return Ok(());
        }
        for shard in fs::read_dir(&blobs)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for b in fs::read_dir(shard.path())? {
                let b = b?;
                let name = b.file_name().to_string_lossy().into_owned();
                if !live.contains(&name) {
                    fs::remove_file(b.path())?;
                }
            }
        }
        Ok(())
    }

    pub fn usage(&self) -> Result<Usage> {
        let live = measure(&self.workspace())?;
        // Snapshot storage grows without bound and is what `prune` controls,
        // so it is reported alongside the live size rather than being
        // invisible until a disk fills.
        let stored = measure(&self.dir.join("blobs"))?;
        Ok(Usage {
            files: live.files,
            bytes: live.bytes,
            snapshots: self.read_meta()?.snapshots.len(),
            snapshot_bytes: stored.bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub files: u64,
    pub bytes: u64,
    pub snapshots: usize,
    /// Bytes held by the blob store across all snapshots, after dedup.
    pub snapshot_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    files: u64,
    bytes: u64,
}

/// SHA-256 of a file, plus its length.
fn hash_file(p: &Path) -> Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    let mut f = fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        len += n as u64;
        h.update(&buf[..n]);
    }
    Ok((format!("{:x}", h.finalize()), len))
}

fn measure(dir: &Path) -> Result<Stats> {
    let mut stats = Stats::default();
    if !dir.exists() {
        return Ok(stats);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            let s = measure(&entry.path())?;
            stats.files += s.files;
            stats.bytes += s.bytes;
        } else if ty.is_file() {
            stats.files += 1;
            stats.bytes += entry.metadata()?.len();
        }
    }
    Ok(stats)
}

/// Keep a volume id to one path segment. A volume id comes from an account
/// identifier, so a `../` in it would place someone else's data inside this
/// volume, or this volume outside the store.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        (d, s)
    }

    fn write(v: &Volume, rel: &str, contents: &str) {
        let p = v.workspace().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    fn read(v: &Volume, rel: &str) -> Option<String> {
        fs::read_to_string(v.workspace().join(rel)).ok()
    }

    #[test]
    fn a_volume_is_created_on_first_use_and_reattached_after() {
        let (_d, s) = store();
        let v = s.volume("user-1").unwrap();
        write(&v, "notes.md", "hello");
        drop(v);

        let again = s.volume("user-1").unwrap();
        assert_eq!(read(&again, "notes.md").as_deref(), Some("hello"));
        assert_eq!(s.volumes().unwrap(), vec!["user-1"]);
    }

    #[test]
    fn a_snapshot_is_immutable_once_taken() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "first");
        let snap = v.snapshot("after first").unwrap();

        write(&v, "a.txt", "second");
        write(&v, "b.txt", "new file");

        // The snapshot must still hold the original bytes and must not have
        // gained the file added afterwards. Checked through restore, which is
        // the path a user relies on.
        v.restore(&snap.id).unwrap();
        assert_eq!(
            read(&v, "a.txt").as_deref(),
            Some("first"),
            "an in-place write to the live file changed the snapshot"
        );
        assert!(read(&v, "b.txt").is_none());
    }

    #[test]
    fn restore_rolls_the_workspace_back_exactly() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "keep.md", "v1");
        write(&v, "gone.md", "will be deleted");
        let snap = v.snapshot("good state").unwrap();

        write(&v, "keep.md", "v2");
        fs::remove_file(v.workspace().join("gone.md")).unwrap();
        write(&v, "added-later.md", "x");

        v.restore(&snap.id).unwrap();

        assert_eq!(read(&v, "keep.md").as_deref(), Some("v1"));
        assert_eq!(read(&v, "gone.md").as_deref(), Some("will be deleted"));
        assert!(
            read(&v, "added-later.md").is_none(),
            "restore left a file the snapshot never had"
        );
    }

    #[test]
    fn restore_takes_a_safety_snapshot_so_it_is_undoable() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "work.md", "a week of work");
        let old = v.snapshot("old").unwrap();

        write(&v, "work.md", "a week of work, plus today");
        let safety = v.restore(&old.id).unwrap();

        // The rollback happened, and is itself reversible.
        assert_eq!(read(&v, "work.md").as_deref(), Some("a week of work"));
        v.restore(&safety.id).unwrap();
        assert_eq!(
            read(&v, "work.md").as_deref(),
            Some("a week of work, plus today"),
            "a mistaken restore was not recoverable"
        );
        assert!(safety.label.contains("before restoring"));
    }

    #[test]
    fn restoring_an_unknown_snapshot_changes_nothing() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "intact");
        let before = v.snapshots().unwrap().len();

        let err = v.restore(&SnapshotId("snap-999999".into()));
        assert!(matches!(err, Err(StoreError::NoSuchSnapshot(_))));
        assert_eq!(read(&v, "a.txt").as_deref(), Some("intact"));
        // No safety snapshot is taken for a restore that never ran.
        assert_eq!(v.snapshots().unwrap().len(), before);
    }

    #[test]
    fn an_interrupted_restore_finishes_on_next_open() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "old");
        v.snapshot("s").unwrap();

        // Simulate a crash after `current` was moved aside and the new tree
        // staged, but before the second rename.
        fs::create_dir_all(v.staging()).unwrap();
        fs::write(v.staging().join("a.txt"), "new").unwrap();
        fs::rename(v.workspace(), v.retired()).unwrap();

        let reopened = s.volume("u").unwrap();
        assert_eq!(
            read(&reopened, "a.txt").as_deref(),
            Some("new"),
            "the staged restore was not completed"
        );
        assert!(!reopened.retired().exists(), "leftover current.old");
        assert!(!reopened.staging().exists(), "leftover current.new");
    }

    #[test]
    fn a_crash_before_staging_completed_keeps_the_old_state() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "old");

        // Moved aside, nothing staged: the old state must come back.
        fs::rename(v.workspace(), v.retired()).unwrap();

        let reopened = s.volume("u").unwrap();
        assert_eq!(read(&reopened, "a.txt").as_deref(), Some("old"));
        assert!(!reopened.retired().exists());
    }

    #[test]
    fn prune_keeps_the_newest() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "x");
        for i in 0..5 {
            v.snapshot(&format!("s{i}")).unwrap();
        }
        assert_eq!(v.snapshots().unwrap().len(), 5);

        assert_eq!(v.prune(2).unwrap(), 3);
        let left = v.snapshots().unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].label, "s3");
        assert_eq!(left[1].label, "s4");
        // Pruning again is a no-op, not an error.
        assert_eq!(v.prune(2).unwrap(), 0);
    }

    /// Count blobs on disk. A collector that collects nothing passes every
    /// test that only checks the survivors, so tests also check that the count
    /// went down.
    fn blob_count_at(store_dir: &std::path::Path) -> usize {
        let mut n = 0;
        let blobs = store_dir.join("volumes/u/blobs");
        let Ok(shards) = fs::read_dir(&blobs) else {
            return 0;
        };
        for shard in shards.flatten() {
            if let Ok(entries) = fs::read_dir(shard.path()) {
                n += entries.flatten().count();
            }
        }
        n
    }

    #[test]
    fn pruning_keeps_every_blob_a_surviving_snapshot_still_needs() {
        // `prune` is the only irreversible operation in this layer.
        // `pruning_reclaims_blobs_nothing_references` gives every snapshot its
        // own content, so an inverted collector that deleted the blobs the
        // doomed snapshots referenced would still pass it: nothing is shared,
        // so nothing live is caught. Shared content is the case that loses
        // data, and it is what this test covers.
        //
        // Checked as an invariant on disk rather than through `restore`.
        // Restore takes a safety snapshot first, which re-hashes the live
        // workspace and puts back any blob still present there, so a restore
        // round trip would repair the damage before observing it.
        let (d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "shared.txt", "same in every snapshot");
        for i in 0..4 {
            write(&v, "changing.txt", &format!("version {i}"));
            v.snapshot(&format!("s{i}")).unwrap();
        }
        let before = blob_count_at(d.path());

        assert_eq!(v.prune(1).unwrap(), 3);

        assert!(
            blob_count_at(d.path()) < before,
            "prune removed no blobs at all, so it is not collecting"
        );
        for snap in v.snapshots().unwrap() {
            for e in v.read_manifest(&snap.id).unwrap().entries {
                assert!(
                    v.blob_path(&e.hash).exists(),
                    "snapshot `{}` still lists {} but its blob was collected",
                    snap.label,
                    e.path
                );
            }
        }
    }

    #[test]
    fn snapshots_taken_at_the_same_moment_do_not_collide() {
        // `snapshot` reads `next_seq`, works for as long as a full copy of the
        // workspace takes, and only then writes the counter back. Without the
        // mutation lock, overlapping snapshots claim the same id and the
        // second manifest overwrites the first: several callers report
        // success and one snapshot exists.
        //
        // Eight at once, each on its own `Volume` handle, the way separate
        // processes would hold them.
        let (d, _s) = store();
        let root = d.path().to_path_buf();
        {
            let s = Store::open(&root).unwrap();
            let v = s.volume("u").unwrap();
            write(&v, "a.txt", "contents");
        }

        let mut threads = Vec::new();
        for i in 0..8 {
            let root = root.clone();
            threads.push(std::thread::spawn(move || {
                let s = Store::open(&root).unwrap();
                let v = s.volume("u").unwrap();
                v.snapshot(&format!("t{i}")).unwrap()
            }));
        }
        let taken: Vec<Snapshot> = threads.into_iter().map(|t| t.join().unwrap()).collect();

        let mut ids: Vec<String> = taken.iter().map(|s| s.id.0.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8, "two snapshots were handed the same id");

        let s = Store::open(&root).unwrap();
        let v = s.volume("u").unwrap();
        let listed = v.snapshots().unwrap();
        assert_eq!(
            listed.len(),
            8,
            "a snapshot reported success and is not in the volume"
        );
        // Each one can still be read back.
        for snap in &listed {
            assert!(!v.read_manifest(&snap.id).unwrap().entries.is_empty());
        }
    }

    #[test]
    fn a_volume_already_being_rewritten_says_so_rather_than_hanging() {
        // The wait must be bounded. A lock with no deadline turns a contended
        // volume into a command that produces nothing, indistinguishable from
        // a hang.
        let (_d, s) = store();
        let holder = s.volume("u").unwrap();
        write(&holder, "a.txt", "x");

        let held = holder
            .lock_mutations()
            .expect("first claim on a free volume");

        let mut waiter = s.volume("u").unwrap();
        waiter.lock_wait = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let out = waiter.snapshot("while the other one runs");
        assert!(
            matches!(out, Err(StoreError::Busy)),
            "expected Busy, got {out:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "it waited far past its own deadline"
        );

        // The volume is usable again once the holder releases it.
        drop(held);
        waiter.snapshot("after").expect("the lock was not released");
    }

    #[test]
    fn snapshots_order_by_sequence_not_by_clock() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "x");
        let a = v.snapshot("first").unwrap();
        let b = v.snapshot("second").unwrap();
        assert!(a.seq < b.seq);
        assert_eq!(v.latest().unwrap().unwrap().id, b.id);
        // Ids sort lexicographically in the same order, so a directory listing
        // is also in sequence order.
        assert!(a.id.as_str() < b.id.as_str());
    }

    #[test]
    fn nested_directories_survive_snapshot_and_restore() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "deep/nested/dir/file.txt", "buried");
        let snap = v.snapshot("deep").unwrap();
        fs::remove_dir_all(v.workspace().join("deep")).unwrap();
        assert!(read(&v, "deep/nested/dir/file.txt").is_none());

        v.restore(&snap.id).unwrap();
        assert_eq!(
            read(&v, "deep/nested/dir/file.txt").as_deref(),
            Some("buried")
        );
    }

    #[test]
    fn usage_counts_live_files_only() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "12345");
        write(&v, "sub/b.txt", "678");
        v.snapshot("s").unwrap();

        let u = v.usage().unwrap();
        assert_eq!(u.files, 2, "snapshot files must not inflate live usage");
        assert_eq!(u.bytes, 8);
        assert_eq!(u.snapshots, 1);
    }

    #[test]
    fn the_browser_profile_is_not_reachable_from_the_workspace() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        let profile = v.browser_profile();
        assert!(
            !profile.starts_with(v.workspace()),
            "the browser profile is inside the workspace, so fs.read can \
             reach its cookies and Login Data: {}",
            profile.display()
        );
        // Still durable: it lives in the volume directory, beside `current`.
        assert!(profile.starts_with(&v.dir));
    }

    #[test]
    fn snapshots_do_not_capture_the_browser_profile() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        fs::create_dir_all(v.browser_profile()).unwrap();
        fs::write(v.browser_profile().join("Cookies"), "secret").unwrap();
        write(&v, "note.md", "work");

        let snap = v.snapshot("s").unwrap();
        assert_eq!(snap.files, 1, "the profile was pulled into a snapshot");
    }

    #[test]
    fn a_volume_id_cannot_escape_the_store() {
        let (d, s) = store();
        let v = s.volume("../../etc").unwrap();
        assert!(
            v.workspace().starts_with(d.path()),
            "volume escaped the store root: {}",
            v.workspace().display()
        );
        assert_eq!(sanitize("../../etc"), "______etc");
    }

    #[test]
    fn an_empty_workspace_snapshots_cleanly() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        let snap = v.snapshot("empty").unwrap();
        assert_eq!(snap.files, 0);
        v.restore(&snap.id).unwrap();
        assert!(v.workspace().exists());
    }
}

#[cfg(test)]
mod content_tests {
    use super::*;
    use std::io::Write;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        (d, s)
    }
    fn write(v: &Volume, rel: &str, contents: &str) {
        let p = v.workspace().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }
    fn read(v: &Volume, rel: &str) -> Option<String> {
        fs::read_to_string(v.workspace().join(rel)).ok()
    }
    fn blob_count(v: &Volume) -> usize {
        let mut n = 0;
        let blobs = v.dir.join("blobs");
        if !blobs.exists() {
            return 0;
        }
        for shard in fs::read_dir(&blobs).unwrap() {
            n += fs::read_dir(shard.unwrap().path()).unwrap().count();
        }
        n
    }

    #[test]
    fn appending_to_a_file_cannot_rewrite_an_earlier_snapshot() {
        // An append opens the existing inode and extends it in place. Nothing
        // is renamed, so a hard-linked snapshot would silently gain the new
        // bytes.
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "log.txt", "line 1\n");
        let snap = v.snapshot("one line").unwrap();

        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(v.workspace().join("log.txt"))
            .unwrap();
        f.write_all(b"line 2\n").unwrap();
        drop(f);
        assert_eq!(read(&v, "log.txt").as_deref(), Some("line 1\nline 2\n"));

        v.restore(&snap.id).unwrap();
        assert_eq!(
            read(&v, "log.txt").as_deref(),
            Some("line 1\n"),
            "an append reached back into a snapshot"
        );
    }

    #[test]
    fn identical_content_is_stored_once_however_many_snapshots_hold_it() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "a.txt", "same bytes");
        write(&v, "copy-of-a.txt", "same bytes");
        write(&v, "b.txt", "different");

        v.snapshot("s1").unwrap();
        assert_eq!(blob_count(&v), 2, "duplicate content was stored twice");

        // Ten more snapshots of unchanged content add nothing.
        for i in 0..10 {
            v.snapshot(&format!("s{i}")).unwrap();
        }
        assert_eq!(blob_count(&v), 2, "unchanged snapshots grew the blob store");

        // A change adds exactly one blob.
        write(&v, "b.txt", "changed");
        v.snapshot("after change").unwrap();
        assert_eq!(blob_count(&v), 3);
    }

    #[test]
    fn pruning_reclaims_blobs_nothing_references() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();

        write(&v, "a.txt", "gen 1");
        v.snapshot("s1").unwrap();
        write(&v, "a.txt", "gen 2");
        v.snapshot("s2").unwrap();
        write(&v, "a.txt", "gen 3");
        let keep = v.snapshot("s3").unwrap();
        assert_eq!(blob_count(&v), 3);

        v.prune(1).unwrap();
        assert_eq!(
            blob_count(&v),
            1,
            "pruned snapshots left their blobs behind"
        );
        // The surviving snapshot still restores.
        v.restore(&keep.id).unwrap();
        assert_eq!(read(&v, "a.txt").as_deref(), Some("gen 3"));
    }

    #[test]
    fn manifest_paths_are_portable_across_platforms() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        write(&v, "deep/nested/file.txt", "x");
        let snap = v.snapshot("s").unwrap();

        let raw = fs::read_to_string(v.manifest_path(&snap.id)).unwrap();
        assert!(
            raw.contains("deep/nested/file.txt"),
            "manifest used platform separators: {raw}"
        );
        assert!(!raw.contains("\\\\"), "backslash leaked into a manifest");
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        (d, s)
    }

    /// Every blob's name must be the hash of its own bytes. If ingest hashed
    /// the source and copied afterwards, a concurrent write would put new
    /// bytes under an old name and this would fail.
    fn audit_blobs(v: &Volume) {
        let blobs = v.dir.join("blobs");
        if !blobs.exists() {
            return;
        }
        for shard in fs::read_dir(&blobs).unwrap() {
            for b in fs::read_dir(shard.unwrap().path()).unwrap() {
                let p = b.unwrap().path();
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                assert!(
                    !name.ends_with(".tmp"),
                    "an ingest temp file was left behind: {name}"
                );
                let (actual, _) = hash_file(&p).unwrap();
                assert_eq!(
                    actual,
                    name,
                    "blob contents do not match its name: {}",
                    p.display()
                );
            }
        }
    }

    #[test]
    fn blobs_always_match_their_own_hash_under_concurrent_writes() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        let ws = v.workspace();
        for i in 0..40 {
            fs::write(ws.join(format!("f{i}.txt")), format!("generation 0 of {i}")).unwrap();
        }

        // A writer churning the same files the snapshot is walking.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let ws = ws.clone();
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut gen = 1u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    for i in 0..40 {
                        let _ = fs::write(
                            ws.join(format!("f{i}.txt")),
                            format!("generation {gen} of {i}"),
                        );
                    }
                    gen += 1;
                }
            })
        };

        for i in 0..8 {
            v.snapshot(&format!("s{i}")).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();

        audit_blobs(&v);

        // Every snapshot must restore to bytes that hash correctly.
        v.force_detach().unwrap();
        for snap in v.snapshots().unwrap() {
            v.restore(&snap.id).unwrap();
            audit_blobs(&v);
        }
    }

    #[test]
    fn a_file_deleted_mid_snapshot_is_simply_not_in_it() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        fs::write(v.workspace().join("stays.txt"), "here").unwrap();
        // A snapshot is a point in time; a file that vanished during the walk
        // was not in it, and must not fail the whole operation.
        let snap = v.snapshot("s").unwrap();
        assert_eq!(snap.files, 1);
    }

    #[test]
    fn restore_is_refused_while_a_guest_holds_the_volume() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        fs::write(v.workspace().join("a.txt"), "x").unwrap();
        let snap = v.snapshot("s").unwrap();

        let held = v.attach().unwrap();
        assert!(v.is_attached());
        assert!(
            matches!(v.restore(&snap.id), Err(StoreError::Attached)),
            "restore swapped the workspace under a running guest"
        );

        drop(held);
        assert!(!v.is_attached(), "the lock outlived its guard");
        v.restore(&snap.id).unwrap();
    }

    /// A plain `--workspace` directory is held the same way a volume is, so
    /// two `openbot up` instances cannot serve agents writing the same files.
    #[test]
    fn two_guests_cannot_hold_one_plain_directory() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("work");

        let first = hold_dir(&dir).expect("the first holds it");
        assert!(
            matches!(hold_dir(&dir), Err(StoreError::Attached)),
            "a second guest took a directory another was already writing"
        );

        drop(first);
        hold_dir(&dir).expect("released when the first let go");
    }

    /// Different directories are different locks.
    #[test]
    fn holding_one_directory_does_not_hold_another() {
        let d = tempfile::tempdir().unwrap();
        let _a = hold_dir(&d.path().join("a")).expect("a");
        hold_dir(&d.path().join("b")).expect("b is a different directory");
    }

    #[test]
    fn two_guests_cannot_hold_one_volume() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        let _first = v.attach().unwrap();
        assert!(matches!(v.attach(), Err(StoreError::Attached)));
    }

    #[test]
    fn a_leftover_lock_file_is_not_a_held_volume() {
        // A dead process cannot be faked from a unit test: forgetting an
        // `Attached` guard leaves the lock genuinely held by this process. What
        // can be tested is a lock file with nobody behind it, such as one left
        // by a version that used the file's existence as the lock.
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        fs::write(v.lock_path(), "99999").unwrap();
        assert!(
            !v.is_attached(),
            "a file nobody holds is being read as a running guest"
        );
        v.force_detach().unwrap();
        assert!(!v.lock_path().exists());
        v.force_detach().unwrap(); // idempotent
    }

    #[test]
    fn a_volume_in_use_will_not_be_force_detached() {
        // Clearing the claim while a guest is running is how two writers end
        // up in one workspace: on Unix the running guest keeps its handle on
        // the unlinked inode, so the next `attach` takes a new file, sees no
        // contention, and both processes believe they own the volume.
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        let _held = v.attach().unwrap();
        assert!(v.is_attached());
        assert!(
            matches!(v.force_detach(), Err(StoreError::Attached)),
            "force-detach tore the volume away from a live guest"
        );
    }

    #[test]
    fn usage_reports_snapshot_storage_not_just_live_bytes() {
        let (_d, s) = store();
        let v = s.volume("u").unwrap();
        fs::write(v.workspace().join("a.txt"), "12345").unwrap();
        v.snapshot("s1").unwrap();
        fs::write(v.workspace().join("a.txt"), "different content").unwrap();
        v.snapshot("s2").unwrap();

        let u = v.usage().unwrap();
        assert_eq!(u.bytes, 17, "live size");
        // Two generations are retained, so stored bytes exceed live bytes.
        assert_eq!(u.snapshot_bytes, 5 + 17);
        assert_eq!(u.snapshots, 2);
    }
}

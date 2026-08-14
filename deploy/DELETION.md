# Deleting an account from the tenant zone

**The one thing to take away: a deletion that ran without error is not a
deletion that worked. The bar is *proven*, and the proof is a thing this
system produces rather than a thing you assume.**

Deletion is the day-one requirement the tenant zone was designed around
(`pd-8lw7`), and it is two acts, each of which is the other's backstop
(`pd-qbrf`):

```
  1. tombstone   registry.sqlite : tenant_key(<id>) -> tombstoned
                 the CRYPTO-SHREDDING. Nothing derives that tenant's key
                 again, so anything the drop missed is ciphertext nobody
                 holds a key for.

  2. drop        tenant/database_id=<id>/  emptied, object by object
                 the ERASURE. One prefix, because database_id sits above
                 dataset= — holdings and valuations go together.

  3. verify      every read path attempted, every one required to fail
```

Neither act is sufficient. **A drop without a tombstone** leaves a live key,
so any copy that survived anywhere is readable. **A tombstone without a drop**
leaves the objects sitting there, and the design says the drop is the erasure.
The verification insists on both, separately, by name.

This is the **offline** half. The online half — releasing the handle, deleting
the collection database and its Litestream replica — is `pkdump tenant detach`
and `pkdump tenant purge`, and it is deliberately a different command in a
different binary: see [`TENANTS.md`](TENANTS.md) and §6 below.

---

## 1. Doing it

```bash
bash deploy/erase.sh prod list   --tenant alice      # what is in the zone
bash deploy/erase.sh prod verify --tenant alice      # ask; changes nothing
bash deploy/erase.sh prod delete --tenant alice --yes --reason "account closed"
```

`--tenant` takes a **handle or a `database_id`**. A handle is resolved through
the registry; a `database_id` is taken as given, because deletion must not
depend on provisioning having been tidy — an id whose registry row is already
gone still has a partition. A name that is neither is a refusal, not a
successful deletion of nothing: a deletion asked for by a typo is far more
likely than one asked for by a name that never existed.

`--yes` is required for `delete` and does not exist for `verify`. There is no
undo: the tombstone is never lifted, no restore of any backup reverses it, and
the objects do not come back.

### Exit status

| | meaning | what to do |
|---|---|---|
| `0` | deleted, and **proven** unreachable on every path | nothing |
| `4` | it **ran** and the deletion is **not proven** | §4 |
| `1` | it could not proceed at all; nothing was deleted | fix and re-run |

**4 is not 1**, for the reason `ship.sh` separates 3 from 1: they need
different first questions. "It never ran" is operational, and retrying is
obviously right. "It ran and cannot be proven" is a question about what is
still reachable, and retrying is not obviously the answer.

Exit 4 raises a Pushover alarm from the wrapper. There is no unit here and
therefore no `OnFailure=` to do it, and a deletion is exactly the operation
somebody starts and walks away from.

### There is no timer, deliberately

Every other container job under `deploy/` is wrapped by a unit and fired by a
calendar. This one is not, and must not be. A deletion is an act somebody
decides to perform on one named account; a scheduled deleter is a thing that
can delete the wrong account at 3am with nobody watching.

---

## 2. What is checked, and why each check is there

```
ok     machinery   the box can still derive SOMEBODY's key
CLOSED derivation  this tenant's key refuses, DELIBERATELY
CLOSED partition   tenant/database_id=<id>/ lists nothing
CLOSED dataset=holdings    …and neither does this dataset
CLOSED dataset=valuations  …nor this one
CLOSED stray copy   a copy taken BEFORE the deletion does not open
```

**`machinery`** is not a read path. It is the precondition that stops the rest
being vacuous: a box with no master key derives nothing for anybody, so "no
key could be derived" would be true of every tenant alive. When there is
another live tenant it derives one as a control, so a refusal below is
demonstrably about *this* tenant and not about this box.

**`derivation`** requires the *specific* refusal, not any refusal. A
tombstoned key and an unregistered id both refuse; only the first is a
deletion. This is `pd-ulds`'s distinction enforced from the reader's side —
accepting any error here would let "we never heard of them" be filed as "we
destroyed their data", and would let a box that lost its key report every
tenant on it as deleted.

**`partition`** is the bulk listing, and the datasets are checked again **by
name**. `Dataset::ALL` is the enumeration a sweep would have to cover, so a
dataset added later and partitioned somewhere the tenant prefix does not reach
is named here rather than averaged into a count that was already zero.

**`stray copy`** is the check the whole crypto-shredding layer exists for, and
it is optional only because most deletions have nobody standing by to take
one. The partition drop has to find every copy; the tombstone does not. So a
copy that survived somewhere — a compacted file, an older snapshot, a bucket
version, a mistake — should be ciphertext nobody holds a key for, and this is
where that is checked against real bytes.

```bash
# take a copy first, then delete against it
aws s3 cp "s3://${BUCKET}/${KEY}" /tmp/copy.enc
PKDUMP_ERASE_STRAY=/tmp/copy.enc bash deploy/erase.sh prod delete \
    --tenant alice --yes --reason "account closed" --stray-key "$KEY"
```

`--stray-key` is required alongside it and is deliberately not guessed: the
object key is the sealed envelope's associated data, so a copy that failed to
open under a key nobody checked would be a proof of nothing. A "copy" that is
not a sealed object makes the check **fail** rather than pass, for the same
reason — a text file does not open either.

### The check answers the other way too

Run `verify` against a tenant who has **not** been deleted and it reports NOT
PROVEN, naming every path that is still open — and it opens the stray copy to
show you. A check that can only ever report success is not a check. Both gates
run it in the failing direction before trusting it in the passing one.

---

## 3. What survives a deletion, honestly

The partition drop removes the objects that are **currently** under the
tenant's prefix. Three things can outlive that, and all three are the reason
the tombstone exists:

* **Bucket versions.** If the lake bucket is versioned, a `DeleteObject`
  leaves a delete marker and the previous versions become noncurrent. They are
  ciphertext under a key that no longer derives, and
  `NoncurrentVersionExpiration: 90` in
  `deploy/policies/tenant-zone/lifecycle.json` removes them within the zone's
  own retention window. The tenant credential is denied
  `s3:PutBucketVersioning`, so this job cannot change that arrangement in
  either direction.
* **Anything already aged past the drop, or copied out by hand.** Same
  answer: unreadable by key, and the 90-day lifecycle bounds how long even the
  ciphertext lives. That is what makes 90 days safer than "indefinite, with a
  delete button".
* **The tenant's own SQLite database**, which this command does not touch at
  all. See §6.

The tombstone is what makes all of that answerable without having to find
every copy by hand — which is precisely a thing nobody can promise to do.

---

## 4. Exit 4: the deletion is not proven

The output names the failing checks. Each has a distinct cause:

| check OPEN | what it means | what to do |
|---|---|---|
| `machinery` | this box cannot derive anybody's key | **Not a tenant problem.** Restore the master key (`deploy/keys.sh <instance> restore`, [`KEYS.md`](KEYS.md) §4), then re-run `verify`. Do not conclude anything about the tenant until it passes. |
| `derivation` | the key still derives, or refuses for the wrong reason | The tombstone is missing. Re-run `delete` — it is idempotent, and it records the tombstone first. |
| `partition` / `dataset=…` | objects remain under the prefix | The drop was interrupted, or something wrote after it. Re-run `delete`; it finishes rather than fails. If it keeps coming back, something is still shipping — check the tombstone (`pkdump keys list`), because the shipper takes the key before it opens a database and a revoked tenant is never shipped. |
| `stray copy` | the copy opened, or could not be judged | If it opened, the key still derives: see `derivation`. If it says *not established*, fix `machinery` first. If it says *envelope magic*, the file being checked is not a sealed zone object and proves nothing either way. |

**Re-running a deletion is safe and is the normal repair.** Tombstoning twice
keeps the first record — *when* a key was destroyed is part of what a
tombstone is for — and dropping an already-empty prefix removes nothing and
succeeds.

---

## 5. If it is interrupted

The order is load-bearing. The tombstone is recorded **before** anything is
removed, so a crash between the two steps leaves:

* a tenant whose key nothing can derive,
* whose remaining objects are therefore ciphertext nobody holds a key for,
* and who the shipper will not ship again.

The state is *more* deleted than intended, never less, and the next run
finishes it. The other order — drop first — would leave an **active** tenant
whose partition had vanished, still deriving and still shipping, and the next
night would put fresh holdings back under a prefix that was supposed to be
gone. The deletion would silently un-happen.

Just run it again.

---

## 6. The other half: the online side

`pkdump-erase` touches the tenant **zone** and the key-state registry. It does
not touch the tenant's collection database, their `user` row, or their
Litestream replica — and it must not, because doing so would mean putting a
tenant-zone credential and the master key inside the online CLI, which is the
coupling the whole zone split exists to prevent.

A full account removal is therefore two commands, and the order that makes
sense is offline first:

```bash
bash deploy/erase.sh prod delete --tenant alice --yes --reason "account closed"

podman exec pkdump-prod pkdump tenant detach alice          # release the handle
podman exec pkdump-prod pkdump tenant purge <database_id> --yes   # the bytes
```

Offline first because the tombstone stops any further shipping immediately,
so nothing new arrives in the zone while the online side is being taken apart.
`purge` is addressed by `database_id` rather than by handle so it cannot be
reached by mistyping a live person's name — see [`TENANTS.md`](TENANTS.md).

---

## 7. The gates

| what | where | tier |
|---|---|---|
| the sweep, the verification, both vacuity guards, every refusal | `crates/pkdump-erase/src/**` (unit) | `rust` |
| the whole path over really-shipped holdings, seen green **and** red | `crates/pkdump-erase/tests/deletion.rs` | `rust` |
| the shipped image against a real bucket, real policies, a real surviving version | `tests/lake/deletion.sh` | `lake` |

The container gate is what proves the parts only a deployment has: that the
binary is in the image at all, that the tenant IAM policy actually permits the
delete and the listing, that a deletion under a **versioned** bucket leaves a
noncurrent version which is genuinely unopenable afterwards, and that
`deploy/erase.sh` maps the job's three exit statuses to the three things an
operator needs to hear.

Every fixture in both is invented data in a throwaway bucket. The tenant zone
is the subject of this design, so its fixtures are treated as if they were
real — and this item's whole job is proving deletion works, which would be
worth nothing if it were demonstrated on somebody's actual holdings.

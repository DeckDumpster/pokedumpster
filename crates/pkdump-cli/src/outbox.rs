//! `pkdump outbox` — emit current holdings as outbox events (pd-385w).
//!
//! ```text
//! pkdump outbox emit --all                     # backfill this collection
//! pkdump outbox emit --all --all-tenants       # ...every registered one
//! pkdump outbox emit --seq 1200..1310          # redrive a lost slice
//! pkdump outbox emit --row 481                 # redrive one holding
//! pkdump outbox status                         # what has been emitted, and when
//! ```
//!
//! **One command over a scope, not three tools.** Backfill, redrive and DR
//! reconcile are the same operation — see `pkdump_db::outbox` for the four
//! rules it obeys and why they are not negotiable. The rare uses run under
//! pressure, at 3am, after something is already broken; a backfill that
//! shares its code with the everyday path has been exercised every day, and
//! a separate `--repair` script has been exercised never.
//!
//! ## The unit of work is the registry, not the current user
//!
//! `--all-tenants` walks `pkdump tenant list` and emits for every registered
//! collection. That flag exists because of `pd-s5yn`: the nightly value
//! snapshot used to run for the one collection `$PKDUMP_USER` resolves to
//! and report success for everybody. A backfill with that shape would arm
//! the shipper against one tenant's outbox and leave every other tenant's
//! holdings invisible to the tenant zone — under-reporting their valuations,
//! silently, exactly as before. **Arming the shipper on prod means running
//! this over every tenant**, which is what `--all-tenants` is for.
//!
//! Under `--all-tenants`, a tenant that fails is named and skipped; the run
//! finishes and exits 2, the same three answers the transform tier gives:
//!
//! | 0 | every tenant emitted |
//! | 2 | some tenant was skipped — a partial run, not a failure |
//! | 1 | the run failed, or never started |
//!
//! Over ONE collection there is no partial state to be in, so a failure is
//! just a failure and exits 1.
//!
//! Paths come from `$PKDUMP_HOME` and `$PKDUMP_USER`, so
//! `podman exec <ctr> pkdump outbox …` works against a running instance the
//! way `pkdump tenant` and `pkdump keys` do.

use pkdump_db::outbox::{self, Emitted, Scope};
use pkdump_db::tenants;

/// Arguments for `pkdump outbox`.
#[derive(clap::Args)]
pub struct OutboxArgs {
    #[command(subcommand)]
    command: OutboxCommand,
}

#[derive(clap::Subcommand)]
enum OutboxCommand {
    /// Emit the current state of a scope as outbox events.
    Emit(EmitArgs),
    /// What has been emitted for a collection, and when.
    Status(WhichArgs),
}

/// Which collections a command works on. Absent, it is the one
/// `$PKDUMP_USER` names — the same default every other subcommand takes.
#[derive(clap::Args)]
pub struct WhichArgs {
    /// One registered tenant, by handle. Refuses a handle nobody holds
    /// rather than provisioning one: a backfill that silently created an
    /// empty collection for a typo would report success over nothing.
    #[arg(long, conflicts_with = "all_tenants")]
    tenant: Option<String>,

    /// Every registered tenant. What arming the shipper on prod needs.
    #[arg(long)]
    all_tenants: bool,
}

#[derive(clap::Args)]
pub struct EmitArgs {
    #[command(flatten)]
    which: WhichArgs,

    /// Every current holding — the backfill, and the DR reconcile.
    #[arg(long, group = "scope")]
    all: bool,

    /// The rows named by outbox events in this range, as `FROM..TO` — the
    /// redrive of a slice the shipper lost.
    #[arg(long, group = "scope", value_name = "FROM..TO")]
    seq: Option<String>,

    /// One holding, by `collection.id`.
    #[arg(long, group = "scope", value_name = "ID")]
    row: Option<i64>,

    /// Run a full backfill that has already been run once. Without it the
    /// second one is refused, naming when the first completed.
    #[arg(long)]
    force: bool,
}

/// Execute `pkdump outbox`.
pub fn run(args: OutboxArgs) -> anyhow::Result<()> {
    match args.command {
        OutboxCommand::Emit(a) => emit(a),
        OutboxCommand::Status(a) => status(a),
    }
}

/// One collection a command is going to work on.
struct Target {
    handle: String,
    path: std::path::PathBuf,
    /// The registry names this file and it is not on this box. Carried
    /// rather than acted on here, because it is fatal for a tenant the
    /// operator named and a skip for one the registry offered.
    absent: bool,
}

/// The collections a `WhichArgs` names.
///
/// **A registry row whose database is not on this box never becomes an open
/// call.** `pkdump_db::open_user` creates what it cannot find, so backfilling
/// a missing file would mint an empty collection, emit nothing from it, and
/// report success — a tenant's holdings silently absent from the zone, which
/// is the exact failure this whole item exists to prevent, reached from the
/// other end.
fn targets(which: &WhichArgs) -> anyhow::Result<Vec<Target>> {
    if which.all_tenants {
        let all = tenants::list()?;
        if all.is_empty() {
            anyhow::bail!(
                "no tenants are registered, so there is nothing to emit. \
                 `pkdump tenant list` to check, `pkdump tenant create` to add one."
            );
        }
        return Ok(all
            .into_iter()
            .map(|t| Target {
                handle: t.user.handle,
                path: t.path,
                absent: !t.present,
            })
            .collect());
    }
    if let Some(handle) = &which.tenant {
        let tenant = tenants::lookup(handle)?.ok_or_else(|| {
            anyhow::anyhow!(
                "no tenant is registered as {handle:?}. `pkdump tenant list` \
                 to see who is."
            )
        })?;
        // Named explicitly, and not there: fatal. The operator asked for
        // this one collection and there is no collection.
        if !tenant.present {
            anyhow::bail!(
                "tenant {:?} is registered as database {} but {} is not on this box",
                tenant.user.handle,
                tenant.user.database_id,
                tenant.path.display()
            );
        }
        return Ok(vec![Target {
            handle: tenant.user.handle,
            path: tenant.path,
            absent: false,
        }]);
    }
    // The default path goes through `resolve`, which is allowed to provision
    // a genuinely fresh data directory — so `absent` is not a question here.
    let collection = crate::collection::resolve()?;
    Ok(vec![Target {
        handle: pkdump_db::current_user(),
        path: collection.path,
        absent: false,
    }])
}

fn scope_of(args: &EmitArgs) -> anyhow::Result<Scope> {
    if args.all {
        return Ok(Scope::Collection);
    }
    if let Some(id) = args.row {
        return Ok(Scope::Row(id));
    }
    if let Some(range) = &args.seq {
        let (from, to) = range.split_once("..").ok_or_else(|| {
            anyhow::anyhow!("--seq takes a range written FROM..TO, not {range:?}")
        })?;
        return Ok(Scope::Seq {
            from: from.trim().parse()?,
            to: to.trim().parse()?,
        });
    }
    // clap's group makes the flags mutually exclusive; it does not make one
    // of them required, and defaulting to `--all` would turn a typo into a
    // full backfill.
    anyhow::bail!("name a scope: --all, --seq FROM..TO, or --row ID")
}

fn emit(args: EmitArgs) -> anyhow::Result<()> {
    let scope = scope_of(&args)?;
    let targets = targets(&args.which)?;
    // Whether a skip is a possible outcome follows what was ASKED FOR, not
    // how many tenants happen to be registered — otherwise `--all-tenants`
    // means one thing on a one-tenant box and another on a two-tenant one,
    // and the exit code a runbook was written against changes when somebody
    // signs up.
    let fleet = args.which.all_tenants;

    println!(
        "emitting scope {} as {} over {} collection{}",
        scope.label(),
        scope.provenance(),
        targets.len(),
        if targets.len() == 1 { "" } else { "s" }
    );

    let mut skipped: Vec<String> = Vec::new();
    let mut total = 0usize;
    for target in &targets {
        let Target {
            handle,
            path,
            absent,
        } = target;
        let outcome = if *absent {
            Err(anyhow::anyhow!(
                "registered, but {} is not on this box — nothing was emitted \
                 and nothing was created",
                path.display()
            ))
        } else {
            emit_one(path, &scope, args.force)
        };
        match outcome {
            Ok(run) => {
                total += run.events;
                println!("  {handle}: {}", describe(&run));
            }
            // One collection has no partial state to be in: whatever went
            // wrong IS the outcome, so it propagates and exits 1. Exit 2
            // means "some of the tenants were skipped", which is only a
            // thing that can happen when there are some.
            Err(e) if !fleet => return Err(e.context(format!("tenant {handle}"))),
            Err(e) => {
                // A tenant mid-import, a restore in flight, a database this
                // build refuses to open. Named and skipped: abandoning the
                // remaining tenants would leave a fleet half-backfilled with
                // nothing saying which half.
                eprintln!("  {handle}: SKIPPED — {e}");
                skipped.push(handle.clone());
            }
        }
    }

    println!(
        "{} event{} emitted across {} collection{}",
        total,
        if total == 1 { "" } else { "s" },
        targets.len() - skipped.len(),
        if targets.len() - skipped.len() == 1 {
            ""
        } else {
            "s"
        }
    );

    if !skipped.is_empty() {
        eprintln!(
            "warning: {} tenant(s) skipped: {}",
            skipped.len(),
            skipped.join(", ")
        );
        // A partial run is its own answer, not a failure and not a success.
        // `std::process::exit` rather than a return type: every connection
        // is already dropped, and the alternative is threading an exit code
        // through every other subcommand for the sake of this one.
        std::process::exit(2);
    }
    Ok(())
}

fn emit_one(path: &std::path::Path, scope: &Scope, force: bool) -> anyhow::Result<Emitted> {
    // `open_user`, not `connect_user`: emitting reads holdings and writes
    // events, and touches no catalog. A backfill must not need the shared
    // catalog to be present or current.
    let mut conn = pkdump_db::open_user(path)?;
    Ok(outbox::emit(&mut conn, scope, force)?)
}

fn describe(run: &Emitted) -> String {
    match (run.seq_first, run.seq_last) {
        (Some(first), Some(last)) => format!(
            "{} event(s) as seq {first}..{last} [{}]",
            run.events,
            run.per_table
                .iter()
                .map(|(t, n)| format!("{t} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => "nothing to emit".to_string(),
    }
}

fn status(which: WhichArgs) -> anyhow::Result<()> {
    for Target {
        handle,
        path,
        absent,
    } in targets(&which)?
    {
        if absent {
            println!("{handle}  registered, but {} is not here", path.display());
            continue;
        }
        let conn = pkdump_db::open_user(&path)?;
        let runs = outbox::runs(&conn)?;
        let pending: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {}", outbox::TABLE),
            [],
            |r| r.get(0),
        )?;

        println!("{handle}  ({pending} event(s) in the outbox)");
        if runs.is_empty() {
            println!(
                "  never emitted — every holding that predates the triggers is \
                 invisible to the tenant zone until `pkdump outbox emit --all` runs"
            );
            continue;
        }
        for run in runs {
            let range = match (run.seq_first, run.seq_last) {
                (Some(a), Some(b)) => format!("seq {a}..{b}"),
                _ => "no events".to_string(),
            };
            println!(
                "  {}  {:<8} {:<16} {} row(s), {range}{}",
                run.completed_at,
                run.source,
                run.scope,
                run.rows_emitted,
                if run.forced { "  [forced]" } else { "" }
            );
        }
    }
    Ok(())
}

use anyhow::{anyhow, bail};
use clap::Clap;
use git_hash::{bstr::ByteSlice, ObjectId};
use git_object::bstr::BString;
use git_odb::find::FindExt;
use git_traverse::commit;
use itertools::Itertools;
use rayon::prelude::*;
use std::{
    ffi::{OsStr, OsString},
    fmt,
    fmt::{Display, Formatter},
    io,
    io::Write,
    path::PathBuf,
    time::Instant,
};

fn is_repo_inner(dir: &OsStr) -> anyhow::Result<()> {
    let p = PathBuf::from(dir).join(".git").canonicalize()?;
    if p.extension().unwrap_or_default() == "git"
        || p.file_name().unwrap_or_default() == ".git"
        || p.join("HEAD").is_file()
    {
        Ok(())
    } else {
        Err(anyhow!(
            "Path '{}' needs to be a directory containing '.git/'",
            p.display()
        ))
    }
}

fn is_repo(dir: &OsStr) -> Result<(), String> {
    is_repo_inner(dir).map_err(|err| err.to_string())
}

#[derive(Clap)]
#[clap(name = "git-hours", about = "Estimate hours worked basted on a commit history", version = clap::crate_version!())]
#[clap(setting = clap::AppSettings::ColoredHelp)]
struct Opts {
    /// The directory containing a '.git/' folder.
    #[clap(parse(from_os_str))]
    #[clap(validator_os = is_repo)]
    working_dir: PathBuf,
    /// The name of the ref like 'main' or 'master' at which to start iterating the commit graph.
    refname: OsString,
    /// Omit personally identifyable information, leaving only the summary.
    #[clap(short = 'p', long)]
    omit_pii: bool,
}

fn main() -> anyhow::Result<()> {
    let opts: Opts = Opts::parse();
    let repo_git_dir = opts.working_dir.join(".git");
    let commit_id = ObjectId::from_hex(
        &std::fs::read(repo_git_dir.join("refs").join("heads").join(opts.refname))?
            .as_bstr()
            .trim(),
    )?;

    let repo_objects_dir = {
        let mut d = repo_git_dir;
        d.push("objects");
        d
    };

    eprintln!("Getting all commits…");
    let start = Instant::now();
    let all_commits = {
        let db = git_odb::linked::Db::at(&repo_objects_dir)?;
        let mut pack_cache = git_odb::pack::cache::Never;
        let mut commits = Vec::<Vec<u8>>::default();
        for commit in commit::Ancestors::new(Some(commit_id), commit::ancestors::State::default(), |oid, buf| {
            db.find_existing(oid, buf, &mut pack_cache).ok().map(|o| {
                commits.push(o.data.to_owned());
                git_object::immutable::CommitIter::from_bytes(o.data)
            })
        }) {
            commit?;
        }
        commits
    };
    let elapsed = start.elapsed();
    eprintln!(
        "Found {} commits and extracted their data in {:?} ({:0.0} commits/s)",
        all_commits.len(),
        elapsed,
        all_commits.len() as f32 / elapsed.as_secs_f32()
    );

    eprintln!("Getting all commit data…");
    let start = Instant::now();
    #[allow(clippy::redundant_closure)]
    let mut all_commits: Vec<CommitInfo> = all_commits
        .into_par_iter()
        .map(|commit_data: Vec<u8>| {
            git_object::immutable::CommitIter::from_bytes(&commit_data)
                .signatures()
                .next()
                .map(|author| git_object::mutable::Signature::from(author))
        })
        .try_fold(
            || Vec::new(),
            |mut out: Vec<_>, item| {
                out.push(item?);
                Some(out)
            },
        )
        .try_reduce(
            || Vec::new(),
            |mut out, vec| {
                out.extend(vec.into_iter());
                Some(out)
            },
        )
        .ok_or_else(|| anyhow!("An error occurred when decoding commits - one commit could not be parsed"))?;
    let elapsed = start.elapsed();
    eprintln!(
        "Obtained {} commits in {:?} ({:0.0} commits/s)",
        all_commits.len(),
        elapsed,
        all_commits.len() as f32 / elapsed.as_secs_f32()
    );
    all_commits.sort_by(|a, b| a.email.cmp(&b.email).then(a.time.time.cmp(&b.time.time).reverse()));
    if all_commits.is_empty() {
        bail!("No commits to process");
    }
    let mut current_email = &all_commits[0].email;
    let mut slice_start = 0;
    let mut results_by_hours = Vec::new();
    for (idx, elm) in all_commits.iter().enumerate() {
        if elm.email != *current_email {
            results_by_hours.push(estimate_hours(&all_commits[slice_start..idx]));
            slice_start = idx;
            current_email = &elm.email;
        }
    }
    if let Some(commits) = all_commits.get(slice_start..) {
        results_by_hours.push(estimate_hours(commits));
    }

    results_by_hours.sort_by(|a, b| a.hours.partial_cmp(&b.hours).unwrap_or(std::cmp::Ordering::Equal));
    if !opts.omit_pii {
        let stdout = io::stdout();
        let mut locked_stdout = stdout.lock();
        for entry in results_by_hours.iter() {
            writeln!(locked_stdout, "{}\n", entry)?;
        }
    }
    let (total_hours, total_commits) = results_by_hours
        .iter()
        .map(|e| (e.hours, e.num_commits))
        .reduce(|a, b| (a.0 + b.0, a.1 + b.1))
        .expect("at least one commit at this point");
    writeln!(
        io::stdout(),
        "total hours: {}, total commits = {}",
        total_hours,
        total_commits
    )?;
    assert_eq!(total_commits, all_commits.len() as u32, "need to get all commits");
    Ok(())
}

fn estimate_hours(commits: &[CommitInfo]) -> WorkByPerson {
    assert!(!commits.is_empty());
    const MAX_COMMIT_DIFFERENCE_IN_MINUTES: f32 = 2.0 * 60.0;
    const FIRST_COMMIT_ADDITION_IN_MINUTES: f32 = 2.0 * 60.0;

    let hours = if commits.len() > 1 {
        commits.iter().rev().tuple_windows().fold(
            0_f32,
            |hours, (cur, next): (&git_object::mutable::Signature, &git_object::mutable::Signature)| {
                let change_in_minutes = (next.time.time - cur.time.time) as f32 / 60.0;
                if change_in_minutes < MAX_COMMIT_DIFFERENCE_IN_MINUTES {
                    hours + change_in_minutes as f32 / 60_f32
                } else {
                    hours + (FIRST_COMMIT_ADDITION_IN_MINUTES / 60.0)
                }
            },
        )
    } else {
        FIRST_COMMIT_ADDITION_IN_MINUTES / 60.0
    };
    let author = &commits[0];
    WorkByPerson {
        name: author.name.to_owned(),
        email: author.email.to_owned(),
        hours,
        num_commits: commits.len() as u32,
    }
}

#[derive(Debug)]
struct WorkByPerson {
    name: BString,
    email: BString,
    hours: f32,
    num_commits: u32,
}

impl Display for WorkByPerson {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} <{}>", self.name, self.email)?;
        writeln!(f, "{} commits found", self.num_commits)?;
        writeln!(
            f,
            "total time spent: {:.02}h ({:.02} 8h days)",
            self.hours,
            self.hours / 8.0
        )
    }
}

type CommitInfo = git_object::mutable::Signature;
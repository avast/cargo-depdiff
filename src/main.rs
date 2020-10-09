use std::cmp;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::fs;
use std::io::ErrorKind;
use std::iter;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Error};
use cargo::util::config::Config;
use cargo_lock::package::{Name, Package, SourceId, Version};
use cargo_lock::Lockfile;
use difference::{Changeset, Difference};
use either::Either;
use git2::{Object, ObjectType, Oid, Repository};
use itertools::{EitherOrBoth, Itertools};
use structopt::StructOpt;
use thiserror::Error;

use sources::Resolver;

mod sources;

/*
 * FIXME: What will happen if package moves from one source to another? When it gets renamed?
 */

/// Checking what changed about dependencies between versions.
#[derive(Debug, StructOpt)]
struct Opts {
    /// Git range specifying the changes to inspect.
    revspec: Option<String>,

    /// Path to the lock file inside the repository.
    ///
    /// Assumes it is the same in both versions.
    #[structopt(short = "p", long = "path", default_value = "Cargo.lock")]
    path: PathBuf,

    /// Path to the git repo with sources.
    #[structopt(short = "g", long = "git-repo", default_value = ".")]
    repo: PathBuf,

    /// Print changes to metadata.
    ///
    /// Will print changes to important metadata:
    ///
    /// * Changed licenses.
    /// * Added authors.
    /// * Addition of build scripts or proc macros (they run during compile time).
    #[structopt(short = "m", long = "metadata")]
    metadata: bool,

    /// Print also additions to CHANGELOG.md when printing metadata.
    ///
    /// Applies only if `-m/--metadata`.
    #[structopt(short = "c", long = "changelog")]
    changelog: bool,
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
struct Dep {
    name: Name,
    version: Version,
    source: Option<SourceId>,
}

impl From<Package> for Dep {
    fn from(pkg: Package) -> Self {
        Self {
            name: pkg.name,
            version: pkg.version,
            source: pkg.source,
        }
    }
}

#[derive(Debug, Error)]
#[error("Not a git blob")]
struct NotBlob;

fn snapshot_to_file_content(repo: &Repository, hash: Oid, path: &Path) -> Result<String, Error> {
    let commit = repo.find_commit(hash)?;
    let tree = commit.tree()?;
    let tree_entry = tree.get_path(path)?;
    let object = tree_entry.to_object(&repo)?;
    let blob = object.as_blob().ok_or(NotBlob)?;
    let content = blob.content();
    String::from_utf8(content.to_owned()).map_err(|_| anyhow!("cannot pase as UTF-8"))
}

type Deps = BTreeMap<Name, Vec<Dep>>;

fn packages_from_str(data: &str) -> Result<Deps, Error> {
    let mut lockfile: Lockfile = data.parse()?;
    lockfile.packages.sort_unstable();

    let mut packages = Deps::new();

    for pkg in lockfile.packages {
        packages
            .entry(pkg.name.clone())
            .or_default()
            .push(pkg.into());
    }

    Ok(packages)
}

fn packages_from_git(repo: &Repository, hash: Oid, path: &Path) -> Result<Deps, Error> {
    let data = snapshot_to_file_content(repo, hash, path)
        .with_context(|| format!("Couldn't find lock file {} in {}", path.display(), hash))?;

    let deps = packages_from_str(&data)
        .with_context(|| format!("{} in {} is not valid lock file", path.display(), hash))?;

    Ok(deps)
}

fn get_changelog(path: &Path) -> Result<String, Error> {
    let changelog_file = path.join("CHANGELOG.md");
    let contents = match fs::read_to_string(changelog_file) {
        Ok(ok) => Ok(ok),
        Err(err) => match err.kind() {
            ErrorKind::NotFound => Ok(String::new()),
            _ => Err(anyhow!("Error while reading CHANGELOG.md")),
        },
    };
    contents
}

fn changelog_diff(old: String, new: String) -> String {
    let changeset = Changeset::new(&old, &new, "\n");
    let mut diff = changeset.diffs.iter().filter_map(|d| match d {
        Difference::Add(a) => Some(a),
        _ => None,
    });
    diff.join("\n")
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Sure, but Update will be much more common
enum Op {
    Add(Dep),
    Remove(Dep),
    Update(Dep, Dep),
}

impl Op {
    fn print_metadata(&self, resolver: &Resolver, changelog: bool) -> Result<(), Error> {
        match self {
            Op::Remove(_) => (), // Removing deps is always good!
            Op::Add(dep) => {
                if let Some(pkg) = resolver.pkg(dep)? {
                    if pkg.has_custom_build() {
                        println!("--> Has a build script");
                    }
                    if pkg.proc_macro() {
                        println!("--> Is a proc macro");
                    }
                }
            }
            Op::Update(old, new) => {
                let old = resolver.pkg(old)?;
                let new = resolver.pkg(new)?;
                if let (Some(old), Some(new)) = (old, new) {
                    if !old.has_custom_build() && new.has_custom_build() {
                        println!("--> Adds a build script");
                    }
                    if !old.proc_macro() && new.proc_macro() {
                        println!("--> Turns into a build script");
                    }

                    let old_meta = old.manifest().metadata();
                    let new_meta = new.manifest().metadata();
                    if old_meta.license != new_meta.license {
                        println!(
                            "--> License changed from {} to {}",
                            old_meta.license.as_deref().unwrap_or("<none>"),
                            new_meta.license.as_deref().unwrap_or("<none>"),
                        );
                    }

                    if old_meta.license_file != new_meta.license_file {
                        println!(
                            "--> License file changed from {} to {}",
                            old_meta.license_file.as_deref().unwrap_or("<none>"),
                            new_meta.license_file.as_deref().unwrap_or("<none>"),
                        );
                    }

                    let old_authors = old_meta.authors.iter().collect::<BTreeSet<_>>();
                    let new_authors = new_meta.authors.iter().collect::<BTreeSet<_>>();
                    let added_authors = &new_authors - &old_authors;

                    if !added_authors.is_empty() {
                        println!(
                            "--> Additional authors ({})",
                            added_authors.iter().join(", ")
                        );
                    }

                    if changelog {
                        let old = get_changelog(old.root());
                        let new = get_changelog(new.root());
                        match (old, new) {
                            (Ok(old), Ok(new)) => {
                                let diff = changelog_diff(old, new);
                                if diff != "" {
                                    println!("--> Additions to CHANGELOG\n{}", diff)
                                }
                            }
                            _ => println!("--> Error while reading CHANGELOG"),
                        }
                    }
                }

                // TODO: We also want maintainers, these are not available through the manifest,
                // but maybe through the crates.io
            }
        }

        Ok(())
    }
    fn print_changelog(&self, resolver: &Resolver) -> Result<(), Error> {
        match self {
            Op::Update(old, new) => {
                let old = resolver.pkg(old)?;
                let new = resolver.pkg(new)?;
                if let (Some(old), Some(new)) = (old, new) {
                    let old = get_changelog(old.root());
                    let new = get_changelog(new.root());
                    match (old, new) {
                        (Ok(old), Ok(new)) => {
                            let diff = changelog_diff(old, new);
                            if diff != "" {
                                println!("--> Additions to CHANGELOG\n{}", diff)
                            }
                        }
                        _ => println!("--> Error while reading CHANGELOG"),
                    }
                }
            }
            _ => (),
        }

        Ok(())
    }
}

impl Display for Op {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        match self {
            Op::Add(dep) => write!(fmt, "+++ {} {}", dep.name, dep.version),
            Op::Remove(dep) => write!(fmt, "--- {} {}", dep.name, dep.version),
            Op::Update(old, new) => {
                write!(fmt, "    {} {} -> {}", old.name, old.version, new.version)
            }
        }
    }
}

fn wrap_op(op: fn(Dep) -> Op, desp: Vec<Dep>) -> impl Iterator<Item = Op> {
    desp.into_iter().map(op)
}

/* FIXME: This should be done differently!
 *
 * --- error-chain 0.12.1
 *     error-chain 0.11.0 -> 0.12.2
 */
fn find_vers_diff(old: Vec<Dep>, new: Vec<Dep>) -> impl Iterator<Item = Op> {
    let mut old: BTreeSet<_> = old.into_iter().collect();
    let mut new: BTreeSet<_> = new.into_iter().collect();

    let unchanged = old.intersection(&new).cloned().collect::<Vec<_>>();

    for u in unchanged {
        old.remove(&u);
        new.remove(&u);
    }

    let mut old = old.into_iter().collect::<Vec<_>>();
    let mut new = new.into_iter().collect::<Vec<_>>();

    let common = cmp::min(old.len(), new.len());
    let removed = old.drain(common..).collect::<Vec<_>>();
    let added = new.drain(common..).collect::<Vec<_>>();

    let common = (old.into_iter().zip(new)).map(|(old, new)| Op::Update(old, new));
    let removed = removed.into_iter().map(Op::Remove);
    let added = added.into_iter().map(Op::Add);

    removed.chain(common).chain(added)
}

#[derive(Error, Debug)]
#[error("Not a git spec")]
struct NotSpec;

#[derive(Error, Debug)]
#[error("No parent to compare to")]
struct NoParent;

fn main() -> Result<(), Error> {
    let opts = Opts::from_args();
    let repo = Repository::open(&opts.repo)
        .with_context(|| format!("Can't open git repo at {}", opts.repo.display()))?;

    let (old, new) = if let Some(revspec) = opts.revspec.as_ref() {
        let spec = |spec: Option<&Object<'_>>| {
            let spec = spec.ok_or(NotSpec)?.id();
            packages_from_git(&repo, spec, &opts.path)
        };

        let revspec = repo.revparse(revspec)?;
        let parent;

        // FIXME: MERGE_BASE mode is not doing the right thing, probably
        let (old_id, new_id) = if revspec.mode().is_range() {
            // a..b mode
            (revspec.from(), revspec.to())
        } else {
            // single-commit
            //
            // Compare to its first parent.
            let commit_obj = revspec.from().ok_or(NotSpec).context("Single commit")?;
            let commit = commit_obj
                .as_commit()
                .with_context(|| format!("{} is not a commit", commit_obj.id()))?;

            parent = commit.parent(0).map_err(|_| NoParent)?.into_object();
            (Some(&parent), Some(commit_obj))
        };

        (
            spec(old_id).context("Failed to decode old version")?,
            spec(new_id).context("Failed to decode new version")?,
        )
    } else {
        let head = repo.head().context("Failed to get current HEAD")?;
        let commit = head
            .peel(ObjectType::Commit)
            .context("Can't resolve HEAD to commit")?
            .id();
        let old = packages_from_git(&repo, commit, &opts.path).context("Reading HEAD lock file")?;

        let path = opts.repo.join(opts.path);
        let current = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read current lock file at {}", path.display()))?;

        let new = packages_from_str(&current)
            .with_context(|| format!("{} is not valid lock file", path.display()))?;

        (old, new)
    };

    let ops = old
        .into_iter()
        .merge_join_by(new, |l, r| l.0.cmp(&r.0))
        .flat_map(|dep_group| match dep_group {
            EitherOrBoth::Left(remove) => Either::Left(wrap_op(Op::Remove, remove.1)),
            EitherOrBoth::Right(add) => Either::Left(wrap_op(Op::Add, add.1)),
            EitherOrBoth::Both(old, new) => Either::Right(find_vers_diff(old.1, new.1)),
        })
        .collect::<Vec<_>>();

    let all_deps = ops.iter().flat_map(|op| match op {
        Op::Add(dep) | Op::Remove(dep) => Either::Left(iter::once(dep)),
        Op::Update(old, new) => Either::Right(iter::once(old).chain(iter::once(new))),
    });

    let config = Config::default()?;
    let resolver = Resolver::new(&config, all_deps)?;

    for op in &ops {
        println!("{}", op);
        if opts.metadata {
            op.print_metadata(&resolver, opts.changelog)?;
        } else if opts.changelog {
            op.print_changelog(&resolver)?;
        }
    }

    Ok(())
}

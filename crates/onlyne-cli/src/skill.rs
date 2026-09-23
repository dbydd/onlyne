//! `onlyne skill` — write the shipped skill documents into a directory tree.
//!
//! The four documents this repository publishes for agents are compiled into
//! this binary, so an installed `onlyne` exports the skills of its own version:
//! no network, no source checkout. `skill export` writes each one to
//! `<dest>/<name>/SKILL.md`, and the default destination is `.agents/skills`
//! under the working directory, the project-local skill root the agent
//! harnesses in this toolchain read. A file whose bytes already match the
//! shipped document is left alone; any other existing file is refused until the
//! operator passes `--force`, the rule `generate` follows for template files.

use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

use crate::flags::GlobalFlags;
use crate::render;
use crate::runtime::{self, EXIT_OK, EXIT_REFUSAL};

/// The leaf every skill directory carries.
const LEAF: &str = "SKILL.md";

/// What the family is, printed in `onlyne skill --help`.
pub const FAMILY_INTRO: &str = "\
Write the skill documents this build ships into a directory tree. The export
reads them out of the binary, so an installed onlyne answers with the skills of
its own version and needs no network and no source checkout. `export` writes
`<dest>/<name>/SKILL.md` for each one, defaults `<dest>` to `.agents/skills`
under the working directory, and refuses to overwrite a file whose bytes differ
until `--force` is passed. Exit codes: 0 for an export that wrote or matched
every selected document, 4 for a refusal naming the file that differs, 2 for a
destination that cannot be written.";

/// One shipped skill: the group it answers to, the directory it lands under, and
/// its bytes.
struct Skill {
    set: SkillSet,
    name: &'static str,
    body: &'static str,
}

/// The group an operator selects with `--set`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SkillSet {
    /// The handbook for a role working its task, with the payload-v2 grammar.
    Role,
    /// The operating manual for a cluster supervisor agent.
    Supervisor,
    /// Development guidance for this repository and its CLI.
    Dev,
}

/// The shipped documents, in the order `export` reports them.
const SHIPPED: [Skill; 4] = [
    Skill {
        set: SkillSet::Supervisor,
        name: "onlyne-supervisor",
        body: include_str!("../skills/onlyne-supervisor/SKILL.md"),
    },
    Skill {
        set: SkillSet::Role,
        name: "onlyne-role",
        body: include_str!("../skills/onlyne-role/SKILL.md"),
    },
    Skill {
        set: SkillSet::Role,
        name: "onlyne-role-payload-v2",
        body: include_str!("../skills/onlyne-role-payload-v2/SKILL.md"),
    },
    Skill {
        set: SkillSet::Dev,
        name: "onlyne",
        body: include_str!("../skills/onlyne/SKILL.md"),
    },
];

/// `onlyne skill <verb>`: the group registered under `Verb::Skill`.
#[derive(Args, Debug, Clone)]
pub struct SkillCmd {
    #[command(subcommand)]
    pub verb: SkillVerb,
}

#[derive(Subcommand, Debug, Clone)]
pub enum SkillVerb {
    /// Write the shipped skill documents under a destination.
    Export(SkillExportArgs),
}

#[derive(Args, Debug, Clone)]
pub struct SkillExportArgs {
    /// Skills root to write into; each document lands at `<dest>/<name>/SKILL.md`.
    /// Defaults to `.agents/skills` under the working directory.
    #[arg(long, value_name = "DIR")]
    pub dest: Option<PathBuf>,
    /// Group to export; repeat the flag for more than one. Every group when omitted.
    #[arg(long = "set", value_enum, value_name = "SET")]
    pub set: Vec<SkillSet>,
    /// Overwrite a file whose bytes differ from the shipped document.
    #[arg(long)]
    pub force: bool,
}

/// What one target needs: a write, or nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Write,
    Unchanged,
}

/// The one line `export` prints.
#[derive(Debug, serde::Serialize)]
struct ExportReport {
    /// Destination the documents were written under.
    dest: String,
    /// The build whose bytes travelled.
    version: &'static str,
    /// Paths written on this run.
    written: Vec<String>,
    /// Paths whose bytes already matched the shipped document.
    unchanged: Vec<String>,
}

/// `skill export`: write every selected document, refusing before any write.
pub fn export(flags: &GlobalFlags, args: &SkillExportArgs) -> i32 {
    let dest = match destination(args) {
        Ok(dest) => dest,
        Err(message) => return runtime::usage_error(message),
    };
    let selected = SHIPPED
        .iter()
        .filter(|skill| args.set.is_empty() || args.set.contains(&skill.set));
    // Every target is read before anything is written, so a refusal names the
    // file that differs and leaves the tree as it stood.
    let mut plan = Vec::new();
    for skill in selected {
        let path = dest.join(skill.name).join(LEAF);
        match std::fs::read(&path) {
            Ok(bytes) if bytes == skill.body.as_bytes() => {
                plan.push((skill, path, Outcome::Unchanged));
            }
            Ok(_) if args.force => plan.push((skill, path, Outcome::Write)),
            Ok(_) => {
                eprintln!(
                    "onlyne: refusing to overwrite {}; pass --force",
                    path.display()
                );
                return EXIT_REFUSAL;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                plan.push((skill, path, Outcome::Write));
            }
            Err(error) => {
                return runtime::usage_error(format!(
                    "onlyne: cannot read {}: {error}",
                    path.display()
                ));
            }
        }
    }
    let mut written = Vec::new();
    let mut unchanged = Vec::new();
    for (skill, path, outcome) in plan {
        if outcome == Outcome::Unchanged {
            unchanged.push(path.display().to_string());
            continue;
        }
        if let Err(message) = write_document(&path, skill.body) {
            return runtime::usage_error(message);
        }
        written.push(path.display().to_string());
    }
    let report = ExportReport {
        dest: dest.display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
        written,
        unchanged,
    };
    let data = serde_json::to_value(&report).expect("a report of paths serialises");
    println!(
        "{}",
        render::render_body(&onlyne_proto::ResBody::ok(data), flags)
    );
    EXIT_OK
}

/// The skills root: `--dest` verbatim, and `.agents/skills` under the working
/// directory when the flag is absent.
fn destination(args: &SkillExportArgs) -> Result<PathBuf, String> {
    match &args.dest {
        Some(dest) => Ok(dest.clone()),
        None => std::env::current_dir()
            .map(|cwd| cwd.join(".agents").join("skills"))
            .map_err(|error| format!("onlyne: cannot read the working directory: {error}")),
    }
}

/// Create the skill directory and write the document into it.
fn write_document(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return Err(format!(
            "onlyne: cannot create {}: {error}",
            parent.display()
        ));
    }
    std::fs::write(path, body)
        .map_err(|error| format!("onlyne: cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{SHIPPED, SkillExportArgs, SkillSet, destination};
    use std::path::PathBuf;

    /// The four documents, with the group each answers to.
    #[test]
    fn the_shipped_table_carries_every_group() {
        let sets: Vec<SkillSet> = SHIPPED.iter().map(|skill| skill.set).collect();
        assert_eq!(
            sets,
            vec![
                SkillSet::Supervisor,
                SkillSet::Role,
                SkillSet::Role,
                SkillSet::Dev
            ]
        );
        for skill in &SHIPPED {
            assert!(
                skill.body.starts_with("---\nname:"),
                "{} carries no skill frontmatter",
                skill.name
            );
        }
    }

    /// `--dest` wins over the working directory.
    #[test]
    fn a_named_destination_stands() {
        let args = SkillExportArgs {
            dest: Some(PathBuf::from("/tmp/skills-root")),
            set: Vec::new(),
            force: false,
        };
        assert_eq!(
            destination(&args).unwrap(),
            PathBuf::from("/tmp/skills-root")
        );
    }

    /// With no `--dest`, the answer is `.agents/skills` under the working
    /// directory, which the test process knows absolutely.
    #[test]
    fn the_default_destination_sits_under_the_working_directory() {
        let args = SkillExportArgs {
            dest: None,
            set: Vec::new(),
            force: false,
        };
        let dest = destination(&args).unwrap();
        assert_eq!(
            dest,
            std::env::current_dir().unwrap().join(".agents/skills")
        );
    }
}

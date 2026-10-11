//! Where the harness finds its ignored inputs: the pinned models and the corpus.
//!
//! Both live under the checkout's ignored `private/` directory and are never committed. A linked
//! worktree has no `private/` of its own, so the lookup falls back to the main checkout's.

use std::path::{Path, PathBuf};

/// The pinned models, relative to a checkout's root.
pub const MODELS_RELATIVE: &str = "private/models/object-removal";
/// The owner's private corpus, relative to a checkout's root.
pub const CORPUS_RELATIVE: &str = "private/object-removal/corpus";
/// The public CC0 subset (fetched by hand, see `corpus.json`), relative to a checkout's root.
pub const PUBLIC_RELATIVE: &str = "private/object-removal/public";

/// The models directory: `--models`, else `LUXFORGE_REMOVAL_MODELS`, else [`MODELS_RELATIVE`].
pub fn models_directory(explicit: Option<PathBuf>) -> PathBuf {
    chosen(explicit, "LUXFORGE_REMOVAL_MODELS", MODELS_RELATIVE)
}

/// The generative candidates' weights, relative to a checkout's root.
pub const GENERATIVE_RELATIVE: &str = "private/models/generative";

/// The generative models' directory: `--gen-models`, else `LUXFORGE_GENERATIVE_MODELS`, else
/// [`GENERATIVE_RELATIVE`].
pub fn generative_directory(explicit: Option<PathBuf>) -> PathBuf {
    chosen(explicit, "LUXFORGE_GENERATIVE_MODELS", GENERATIVE_RELATIVE)
}

/// The private corpus directory: `--corpus`, else `LUXFORGE_REMOVAL_CORPUS`, else
/// [`CORPUS_RELATIVE`].
pub fn corpus_directory(explicit: Option<PathBuf>) -> PathBuf {
    chosen(explicit, "LUXFORGE_REMOVAL_CORPUS", CORPUS_RELATIVE)
}

/// The public subset's directory: `--public`, else `LUXFORGE_REMOVAL_PUBLIC`, else
/// [`PUBLIC_RELATIVE`].
pub fn public_directory(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os("LUXFORGE_REMOVAL_PUBLIC").map(PathBuf::from))
        .unwrap_or_else(|| {
            // Beside the private corpus when that exists, wherever it was found.
            let parent = find_private(PUBLIC_RELATIVE.rsplit_once('/').map_or("", |(p, _)| p));
            let public = parent.join("public");
            if parent.is_dir() {
                public
            } else {
                PathBuf::from(PUBLIC_RELATIVE)
            }
        })
}

fn chosen(explicit: Option<PathBuf>, variable: &str, relative: &str) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os(variable).map(PathBuf::from))
        .unwrap_or_else(|| find_private(relative))
}

/// `relative` under the nearest checkout (the current directory's ancestors, then the main
/// checkout of a linked worktree) that has it; else `relative` itself, so a message names it.
pub fn find_private(relative: &str) -> PathBuf {
    let Ok(cwd) = std::env::current_dir() else {
        return PathBuf::from(relative);
    };
    for dir in cwd.ancestors() {
        let candidate = dir.join(relative);
        if candidate.exists() {
            return candidate;
        }
        if let Some(main) = main_checkout(dir) {
            let candidate = main.join(relative);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(relative)
}

/// The main checkout's root, when `dir` is the root of a linked worktree: its `.git` file names
/// `<main>/.git/worktrees/<name>`, whose `commondir` file points back at `<main>/.git`.
fn main_checkout(dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dir.join(".git")).ok()?;
    let git_dir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        dir.join(git_dir)
    };
    let common = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    let common = git_dir.join(common.trim());
    common.canonicalize().ok()?.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_arguments_win_and_the_default_names_the_relative_path() {
        assert_eq!(
            models_directory(Some(PathBuf::from("/somewhere"))),
            PathBuf::from("/somewhere")
        );
        // Without an override or a checkout that holds it, the message can still name the path.
        assert!(
            find_private("private/no-such-directory-for-this-test")
                .ends_with("private/no-such-directory-for-this-test")
        );
    }

    #[test]
    fn a_linked_worktree_finds_the_main_checkouts_private_directory() {
        let root = std::env::temp_dir().join(format!("luxforge-paths-{}", std::process::id()));
        let main = root.join("main");
        let worktree = root.join("linked");
        let gitdir = main.join(".git/worktrees/linked");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        let found = main_checkout(&worktree).expect("the main checkout");
        assert_eq!(found, main.canonicalize().unwrap());
        assert_eq!(
            main_checkout(&main),
            None,
            "a main checkout has a .git directory"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

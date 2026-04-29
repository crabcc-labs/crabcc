use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub fn walk_repo(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(true)
        .git_ignore(true)
        .build()
        .filter_map(|r| r.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.into_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    fn rel_paths(root: &Path) -> HashSet<String> {
        walk_repo(root)
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn walks_basic_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.ts"), "export const x = 1;").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.rb"), "class B; end").unwrap();

        let paths = rel_paths(root);
        assert!(paths.contains("a.ts"), "missing a.ts; got {paths:?}");
        assert!(
            paths.contains("sub/b.rb"),
            "missing sub/b.rb; got {paths:?}"
        );
    }

    #[test]
    fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Without a `.git` dir or repo init, `ignore` only respects `.gitignore`
        // when also given a `.git` parent — emulate via `.ignore`, which the
        // crate honors regardless. Either file should hide `secret.ts`.
        fs::write(root.join(".ignore"), "secret.ts\n").unwrap();
        fs::write(root.join("a.ts"), "ok").unwrap();
        fs::write(root.join("secret.ts"), "shh").unwrap();

        let paths = rel_paths(root);
        assert!(paths.contains("a.ts"));
        assert!(
            !paths.contains("secret.ts"),
            "secret.ts should be ignored; got {paths:?}"
        );
    }

    #[test]
    fn excludes_hidden_dotfiles_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::create_dir(root.join(".cache")).unwrap();
        fs::write(root.join(".cache/c.ts"), "leak").unwrap();
        fs::write(root.join("a.ts"), "ok").unwrap();

        let paths = rel_paths(root);
        assert!(paths.contains("a.ts"));
        assert!(!paths.contains(".env"), "hidden .env leaked: {paths:?}");
        assert!(
            !paths.contains(".cache/c.ts"),
            "hidden dir contents leaked: {paths:?}"
        );
    }

    #[test]
    fn empty_dir_returns_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = rel_paths(dir.path());
        assert_eq!(paths.len(), 0);
    }
}

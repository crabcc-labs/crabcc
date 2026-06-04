use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub fn walk_repo(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(true)
        .git_ignore(true)
        // Compose extra ignore files on top of .gitignore/.ignore (already
        // honored by standard_filters): .dockerignore (skip build-context
        // excludes) then .cccignore (crabcc-specific overrides — added last so
        // it has the highest precedence and can re-include with `!`). Fewer
        // files walked = faster index + load.
        .add_custom_ignore_filename(".dockerignore")
        .add_custom_ignore_filename(".cccignore")
        .build()
        .filter_map(|r| r.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.into_path())
        .filter(|p| !is_python_bytecode(p))
}

/// Skip Python bytecode (`*.pyc`, `*.pyo`) and anything inside a
/// `__pycache__/` directory, regardless of the target repo's gitignore.
/// These are generated artifacts; no symbol extractor should see them.
fn is_python_bytecode(path: &Path) -> bool {
    if matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("pyc") | Some("pyo")
    ) {
        return true;
    }
    path.components().any(|c| c.as_os_str() == "__pycache__")
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
    fn respects_cccignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".cccignore"), "generated/\n*.min.js\n").unwrap();
        fs::write(root.join("a.ts"), "ok").unwrap();
        fs::write(root.join("bundle.min.js"), "x").unwrap();
        fs::create_dir(root.join("generated")).unwrap();
        fs::write(root.join("generated/g.ts"), "gen").unwrap();

        let paths = rel_paths(root);
        assert!(paths.contains("a.ts"));
        assert!(
            !paths.contains("bundle.min.js"),
            ".cccignore glob not applied: {paths:?}"
        );
        assert!(
            !paths.contains("generated/g.ts"),
            ".cccignore dir not applied: {paths:?}"
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

    #[test]
    fn skips_python_bytecode_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.py"), "x = 1").unwrap();
        fs::create_dir(root.join("__pycache__")).unwrap();
        fs::write(root.join("__pycache__/a.cpython-314.pyc"), b"\x00\x00").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.py"), "y = 2").unwrap();
        fs::create_dir(root.join("sub/__pycache__")).unwrap();
        fs::write(root.join("sub/__pycache__/b.pyo"), b"\x00\x00").unwrap();
        fs::write(root.join("loose.pyc"), b"\x00").unwrap();

        let paths = rel_paths(root);
        assert!(paths.contains("a.py"));
        assert!(paths.contains("sub/b.py"));
        for p in &paths {
            assert!(
                !p.contains("__pycache__"),
                "__pycache__ leaked into walker: {p:?}"
            );
            assert!(
                !p.ends_with(".pyc") && !p.ends_with(".pyo"),
                "bytecode leaked: {p:?}"
            );
        }
    }
}
